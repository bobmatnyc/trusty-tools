//! The rendered CAST §1 methodology row reflects the run, not the template (#6675).
//!
//! Why: the row was fixed template text asserting trusty-analyze and
//! trusty-search were both used. The 2026-09-02 dogfood run fell back to scan
//! and anchored no symbol, and a reader who stopped at §1 was still told both
//! tools had run. This asserts the rendered row against a run where neither
//! lane contributed, which is the case the fixed text got wrong.
//! What: renders the bundled CAST template over a fixture repository with no
//! trusty-analyze metrics and no investigation, and asserts the row names both
//! absences and carries no unqualified tool-use claim; then renders the same
//! fixture WITH metrics and asserts the analyze lane is named as used.
//! Test: this file.
#![cfg(feature = "report")]

use std::path::Path;

use trusty_review::report::{Reporter, TemplateLoader, load_manifest, model::ReportModel};

/// The bundled template name under test.
const CAST: &str = "report-technical-dd-cast";

/// The exact claim the fixed row made on every run, whatever ran.
const FIXED_CLAIM: &str = "Repository inspection via trusty-analyze (static code analysis, \
                           structural metrics, complexity measurement) + trusty-search \
                           (architecture context, KG-guided focus)";

/// A metrics artifact the manifest can declare, so the analyze lane has data.
const FIXTURE_METRICS: &str = r#"{
  "schema_version": "v0",
  "repository": "fixture",
  "loc": { "total": 3, "by_language": [ { "language": "Python", "loc": 3 } ] },
  "counts": { "files": 1, "functions": 1 },
  "complexity": { "buckets": [ { "label": "low (1-5)", "count": 1 } ] },
  "findings": []
}"#;

fn write_fixture_repo(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/app.py"),
        "def total(items):\n    return sum(items)\n",
    )
    .expect("write app.py");
}

/// Render the bundled CAST template over the fixture, with or without a
/// declared trusty-analyze metrics artifact.
fn render(with_metrics: bool) -> String {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let repo = dir.join("fixture-repo");
    write_fixture_repo(&repo);

    let metrics_line = if with_metrics {
        std::fs::write(dir.join("metrics.json"), FIXTURE_METRICS).expect("write metrics");
        "metrics = \"metrics.json\"\n"
    } else {
        ""
    };
    let manifest_toml = format!(
        "[report]\n\
         title = \"Fixture Technical DD\"\n\
         template = \"cast\"\n\
         analyst = \"bobmatnyc\"\n\n\
         [[repositories]]\n\
         name = \"Fixture App\"\n\
         path = \"{}\"\n\
         {metrics_line}",
        repo.display()
    );
    let manifest_path = dir.join("manifest.toml");
    std::fs::write(&manifest_path, manifest_toml).expect("write manifest");

    let manifest = load_manifest(&manifest_path).expect("manifest loads");
    let template = TemplateLoader::bundled_only()
        .load(CAST)
        .expect("bundled CAST template loads");
    let mut model =
        ReportModel::build(&manifest, &manifest_path, CAST, None).expect("model builds");
    // #5454: `write`/`render` require a completed synthesis pass; this test is
    // about the deterministic §1 fill, so an empty pass stands in for the LLM.
    model.synthesis = Some(trusty_review::report::Synthesis::default());

    Reporter::new(dir.join("reports")).render(&model, &template)
}

/// The live defect: a run where neither lane contributed still printed the
/// fixed both-tools claim.
///
/// Fails before the fix: the row is template literal text, so `FIXED_CLAIM` is
/// present in the render regardless of what the run recorded.
#[test]
fn the_methodology_row_states_a_run_where_neither_lane_contributed() {
    let md = render(false);
    assert!(
        !md.contains(FIXED_CLAIM),
        "the fixed both-tools claim must not survive a run that used neither tool:\n{md}"
    );
    assert!(
        md.contains("trusty-analyze contributed no data to this run"),
        "the row must name the analyze lane's absence:\n{md}"
    );
    assert!(
        md.contains("trusty-search symbol tracing did not run"),
        "the row must name the search lane's absence:\n{md}"
    );
}

/// The row is not a blanket denial either: a run whose analyze lane did land
/// says so, with its own count.
#[test]
fn the_methodology_row_credits_an_analyze_lane_that_landed() {
    let md = render(true);
    assert!(
        md.contains("trusty-analyze structural metrics for all 1 application(s)"),
        "the row must credit the analyze lane that contributed:\n{md}"
    );
}
