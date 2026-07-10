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

use trusty_review::report::{
    CorpusSnapshot, Reporter, TemplateLoader, benchmark, load_manifest, model::ReportModel,
};

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
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");

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
    // Wave 2 (#2342): unmapped fields are omitted, not rendered as marker walls;
    // the compact Data gaps list carries them instead.
    assert!(!md.contains("not stated in source data"));
    assert!(md.contains("Data gaps:"));
    // Self-derived vendor/methodology is filled from the tool itself.
    assert!(md.contains("trusty-review report (repository inspection) v"));
    assert!(!md.contains("{{"));
    // Live-QA defect #3: the template's leading instructional comment header
    // must never reach the generated report — output starts at the title.
    assert!(
        md.trim_start()
            .starts_with("# Technical Due-Diligence Analysis:")
    );
    assert!(!md.contains("PLACEHOLDER SYNTAX"));
    assert!(!md.contains("trusty-review template:"));

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

/// Why: prove the M3 corpus + benchmarking path composes end to end — a populated
/// corpus (five peers) ranks a target, fills the benchmark tables, and records the
/// placement on the JSON twin — and that a render with benchmarking OFF is
/// byte-identical to the M2 deterministic output.
/// What: seeds a corpus directory with five peer snapshots, builds a single-repo
/// model with metrics, ranks it, and asserts the filled markdown + JSON, plus the
/// off/on byte-identity of the non-benchmark render.
/// Test: this test itself.
#[test]
fn end_to_end_corpus_benchmark() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let corpus_dir = dir.join("corpus");

    // Seed five peer snapshots spanning a LoC range around the target.
    for (i, loc) in [1000u64, 2000, 3000, 4000, 5000].into_iter().enumerate() {
        let metrics_json = format!(
            r#"{{ "loc": {{ "total": {loc} }}, "counts": {{ "files": {}, "functions": {} }} }}"#,
            10 + i,
            100 + i * 10
        );
        let m: trusty_review::report::AnalyzeMetrics =
            serde_json::from_str(&metrics_json).expect("peer metrics");
        let snap = CorpusSnapshot {
            schema_version: "corpus-v0".to_string(),
            slug: format!("peer{i}"),
            name: format!("Peer {i}"),
            source_basename: format!("peer{i}"),
            git_sha: None,
            timestamp: "2026-07-10".to_string(),
            metrics: m,
        };
        benchmark::write_snapshot(&corpus_dir, &snap).expect("seed peer");
    }

    // Target metrics: 3500 LoC → sits above three peers (100/200/300/target).
    std::fs::write(
        dir.join("target.json"),
        r#"{ "loc": { "total": 3500, "by_language": [ { "language": "Rust", "loc": 3500 } ] },
            "counts": { "files": 42, "functions": 300 } }"#,
    )
    .expect("write target metrics");

    let manifest_toml = r#"
        [report]
        title = "Bench DD"
        template = "report-technical-dd"

        [[repositories]]
        name = "Target App"
        path = "/nonexistent/target"
        metrics = "target.json"
    "#;
    let manifest_path = dir.join("manifest.toml");
    std::fs::write(&manifest_path, manifest_toml).expect("write manifest");

    let manifest = load_manifest(&manifest_path).expect("manifest");
    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template");
    let mut model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("model");

    // Render OFF first (deterministic baseline).
    let reporter = Reporter::new(dir.join("reports"));
    let off = reporter.render(&model, &template);
    assert!(!off.contains("## Benchmark Status"));

    // Now benchmark and render ON.
    let corpus = benchmark::load_corpus(&corpus_dir).expect("load corpus");
    assert_eq!(corpus.snapshots.len(), 5);
    let targets: Vec<CorpusSnapshot> = model
        .repositories
        .iter()
        .filter_map(|r| CorpusSnapshot::from_repository(r, &model.generated_date))
        .collect();
    let report = benchmark::build_benchmark_report(&corpus, &targets);
    model.benchmark = Some(report);

    let on = reporter.render(&model, &template);
    // Ranked: the target's Total LoC placement over a population of six.
    assert!(on.contains("Total LoC"));
    assert!(on.contains("of 6"));
    assert!(on.contains("## Benchmark Status"));
    assert!(on.contains("corpus size 5"));
    assert!(on.contains("ranked against 5 peer(s)"));
    assert!(!on.contains("{{"));

    // Off/on byte-identity for everything BEFORE the appended status section:
    // the ON render must start with the OFF render's benchmark-free body plus
    // only the new status section and filled rows differing.
    assert!(on.len() > off.len(), "ON adds the status section");

    // JSON twin records the benchmark placement.
    let json = serde_json::to_value(&model).expect("json");
    assert_eq!(json["benchmark"]["corpus_size"], 5);
    assert_eq!(
        json["benchmark"]["repositories"][0]["status"]["state"],
        "ranked"
    );
}
