//! End-to-end deterministic report generation (M1, #2313).
//!
//! Why: the unit tests cover each stage in isolation; this integration test
//! proves the whole pipeline composes — a fixture manifest + fixture metrics JSON
//! render, through the public API, into a markdown + JSON report pair with the
//! expected content and the honesty rule applied.
//! What: writes a fixture manifest and metrics file into a temp dir, runs
//! `load_manifest` → `ReportModel::build` → `Reporter::write`, and asserts the
//! rendered markdown and the written files.
//! Test: this file (only compiled with the default `report` feature).
#![cfg(feature = "report")]

use trusty_review::report::{Reporter, TemplateLoader, load_manifest, model::ReportModel};

/// Why: prove the full manifest → model → render → write path end to end.
/// What: builds a two-repo report and asserts both apps, metrics-derived values,
/// the honesty marker, and the two written output files.
/// Test: this test itself.
#[test]
fn end_to_end_two_repo_report() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();

    // Fixture metrics for the first application.
    std::fs::write(
        dir.join("web.json"),
        r#"{
          "loc": { "total": 8200, "by_language": [
            { "language": "TypeScript", "loc": 6000 },
            { "language": "CSS", "loc": 2200 }
          ]},
          "counts": { "files": 120, "functions": 640 }
        }"#,
    )
    .expect("write web metrics");

    // Fixture manifest: one local (with metrics) + one remote (declared only).
    let manifest_toml = r#"
        [report]
        title = "Northwind Technical DD"
        template = "report-technical-dd"
        analyst = "bobmatnyc"

        [[repositories]]
        name = "Northwind Web"
        path = "/nonexistent/northwind-web"
        metrics = "web.json"

        [[repositories]]
        name = "Northwind API"
        remote = "northwind/api"
        username = "bobmatnyc"
        ref = "main"
    "#;
    let manifest_path = dir.join("manifest.toml");
    std::fs::write(&manifest_path, manifest_toml).expect("write manifest");

    // Run the pipeline through the public API.
    let manifest = load_manifest(&manifest_path).expect("manifest loads");
    assert_eq!(manifest.repositories.len(), 2);

    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd").expect("model builds");

    let out_dir = dir.join("reports");
    let reporter = Reporter::new(&out_dir);
    let md = reporter.render(&model, &template);

    // Report-level and per-application content.
    assert!(md.contains("Northwind Technical DD"));
    assert!(md.contains("Northwind Web"));
    assert!(md.contains("Northwind API"));
    assert!(md.contains("bobmatnyc"));
    // Metrics-derived fields for the local app.
    assert!(md.contains("8200"));
    assert!(md.contains("TypeScript"));
    assert!(md.contains("120 files, 640 functions"));
    // Honesty rule: unmapped scoring fields are marked, never left as {{...}}.
    assert!(md.contains("not stated in source data"));
    assert!(!md.contains("{{"));

    // Write and verify the dual-output pair.
    let written = reporter.write(&model, &template).expect("write ok");
    assert_eq!(written.len(), 2);
    for path in &written {
        assert!(path.exists(), "missing output {}", path.display());
    }

    // JSON twin round-trips and records both repositories + git enrichment shape.
    let json_path = written
        .iter()
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .expect("json output");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(json_path).expect("read json"))
            .expect("valid json");
    assert_eq!(json["title"], "Northwind Technical DD");
    assert_eq!(json["repositories"].as_array().unwrap().len(), 2);
    assert_eq!(json["repositories"][1]["source_kind"], "remote");
}
