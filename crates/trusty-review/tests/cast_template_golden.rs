//! Every CAST heading survives a code-only render (#6669).
//!
//! Why: the guarantee this whole mode exists for is that a reader can tell a
//! deliberate boundary from a silent omission. That only holds if EVERY heading
//! the CAST outline carries is still on the page after a code-only render, and
//! if each non-code section says why it is empty. A wording change in a section
//! instruction must not be able to break this test, so it asserts heading
//! strings and the boundary sentence rather than diffing bytes.
//! What: renders the bundled CAST template against a small two-language fixture
//! repository, in code-only mode, and asserts (a) every `#` heading the template
//! declares appears in the output, (b) the two non-code sections carry the
//! out-of-scope block, (c) the partial sections carry the provenance line, and
//! (d) the #6004 Code Quality / Security / Performance sections are present.
//! Test: this file.
#![cfg(feature = "report")]

use std::path::Path;

use trusty_review::report::{
    Reporter, TemplateLoader, code_only, load_manifest, model::ReportModel,
};

/// The bundled template name under test.
const CAST: &str = "report-technical-dd-cast";

/// Headings the CAST outline must always carry, verbatim.
///
/// Why: derived from the template itself, but pinned here as literals so a
/// heading DELETED from the template fails this test rather than silently
/// shrinking the expectation with it.
const REQUIRED_HEADINGS: &[&str] = &[
    "# CAST Technical Due-Diligence Analysis:",
    "## 1. Report Metadata",
    "## 2. Executive Summary",
    "### Top Risks",
    "## 3. CAST Scoring Model & Normalization",
    "## 4. Per-Application Scorecard",
    "## 5. Findings by Severity",
    "### 5.1 RED / CRITICAL Findings",
    "### 5.2 AMBER / MEDIUM Findings",
    "### 5.3 GREEN / POSITIVE Findings",
    "## Code Quality & Architecture",
    "## Security Posture",
    "## Performance & Scalability",
    "## 6. Risk Registers",
    "### 6.1 ISO-5055 Compliance",
    "### 6.2 Open-Source & CVE Exposure",
    "### 6.3 License / IP Risk",
    "### 6.4 Open-Source Component Obsolescence",
    "### 6.5 Cloud-Native Compliance / PaaS Maturity",
    "### 6.6 Green Impact / Sustainability Scan",
    "### 6.7 Remediation Economics",
    "## 7. Graph-Ready Data Appendix",
    "## 8. Ticketing & Delivery Traceability",
    "## 9. Gaps & Caveats",
    "## 10. Next Steps",
];

/// A small fixture repository: two languages, one hardcoded-looking secret, one
/// pinned dependency.
fn write_fixture_repo(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/auth.py"),
        "API_KEY = \"not-a-real-key-fixture\"\n\n\
         def authenticate(user, token):\n    \
         if not token:\n        return False\n    \
         return token == API_KEY\n",
    )
    .expect("write auth.py");
    std::fs::write(
        root.join("src/index.js"),
        "export function total(items) {\n  \
         return items.reduce((a, b) => a + b.price, 0);\n}\n",
    )
    .expect("write index.js");
    std::fs::write(
        root.join("package.json"),
        "{\n  \"name\": \"fixture\",\n  \"dependencies\": { \"left-pad\": \"1.0.0\" }\n}\n",
    )
    .expect("write package.json");
}

/// A metrics artifact with one finding in each severity band.
///
/// Why: the polish pass collapses a section whose every row was dropped, so a
/// findings-free fixture would leave §5.1/5.2/5.3 collapsed into their parent
/// and this test would be asserting the empty-section rule rather than the
/// code-only one. One finding per band is what makes the three sub-headings
/// render for real.
const FIXTURE_METRICS: &str = r#"{
  "schema_version": "v0",
  "repository": "fixture",
  "loc": { "total": 8, "by_language": [
    { "language": "Python", "loc": 5 },
    { "language": "JavaScript", "loc": 3 }
  ]},
  "counts": { "files": 3, "functions": 2 },
  "complexity": { "buckets": [ { "label": "low (1-5)", "count": 2 } ] },
  "findings": [
    { "title": "Credential in source", "severity": "red", "category": "security",
      "component": "src/auth.py", "description": "A literal API key is assigned at module scope.",
      "remediation": "Read it from the environment or a secret store." },
    { "title": "Unpinned transitive dependency", "severity": "amber", "category": "dependencies",
      "component": "package.json", "description": "left-pad is pinned to an exact old version.",
      "remediation": "Move to a maintained range and re-lock." },
    { "title": "Consistent module layout", "severity": "green", "category": "maintainability",
      "component": "src/", "description": "Sources sit under one directory.",
      "remediation": "None." }
  ]
}"#;

/// Render the bundled CAST template over the fixture repository.
fn render(code_only_mode: bool) -> String {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path();
    let repo = dir.join("fixture-repo");
    write_fixture_repo(&repo);

    std::fs::write(dir.join("metrics.json"), FIXTURE_METRICS).expect("write metrics");
    let manifest_toml = format!(
        "[report]\n\
         title = \"Fixture Technical DD\"\n\
         template = \"cast\"\n\
         analyst = \"bobmatnyc\"\n\
         client = \"Fixture Holdings\"\n\n\
         [[repositories]]\n\
         name = \"Fixture App\"\n\
         path = \"{}\"\n\
         metrics = \"metrics.json\"\n",
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
    // #5454: `write` requires a completed synthesis pass. This test is about
    // the deterministic fill and the code-only transform, so an empty pass
    // stands in for the LLM it deliberately does not call.
    model.synthesis = Some(trusty_review::report::Synthesis::default());

    Reporter::new(dir.join("reports"))
        .with_code_only(code_only_mode)
        .render(&model, &template)
}

/// Why: the whole guarantee — nothing is silently omitted. A reader must find
/// every CAST section on the page even when the audit could not fill it.
/// What: every heading in [`REQUIRED_HEADINGS`] appears in a code-only render.
/// Test: this test itself.
#[test]
fn every_cast_heading_survives_a_code_only_render() {
    let md = render(true);
    for heading in REQUIRED_HEADINGS {
        assert!(
            md.contains(heading),
            "code-only render dropped the heading {heading:?}.\n--- rendered ---\n{md}"
        );
    }
}

/// Why: a heading with an empty table under it reads as a measurement that came
/// back clean. The two sections nothing in a repository can answer must say
/// why instead.
/// What: the Peer Benchmark block and the Next Steps section both carry the
/// out-of-scope lead, and each names its own reason.
/// Test: this test itself.
#[test]
fn the_non_code_sections_state_their_boundary() {
    let md = render(true);
    assert_eq!(
        md.matches(code_only::OUT_OF_SCOPE_LEAD).count(),
        2,
        "expected exactly the Peer Benchmark and Next Steps boundaries.\n--- rendered ---\n{md}"
    );
    assert!(
        md.contains("CAST's proprietary reference corpus"),
        "the Peer Benchmark boundary must name the corpus it needs:\n{md}"
    );
    assert!(
        md.contains("interviews with the delivery organization"),
        "the Next Steps boundary must name the interviews it needs:\n{md}"
    );
    // The boundary replaces the data, so no benchmark placeholder may survive
    // to be honesty-marked in its place.
    assert!(
        !md.contains("Quartile"),
        "no peer-benchmark table may render under code-only:\n{md}"
    );
}

/// Why: a section that IS code-derived but never cross-checked must not read as
/// validated. The provenance line is the only thing distinguishing the two.
/// What: the three partial sections each carry the note.
/// Test: this test itself.
#[test]
fn the_partial_sections_carry_the_provenance_line() {
    let md = render(true);
    assert_eq!(
        md.matches(code_only::PARTIAL_NOTE).count(),
        3,
        "expected the CVE, license and remediation-economics notes.\n--- rendered ---\n{md}"
    );
}

/// Why (#6669): the metadata table is where a reader checks what this document
/// claims to be. A code-only report that does not say so is the failure this
/// whole mode is about.
/// What: the Audit scope row states the code-only scope, and a full-scope
/// render states the other one.
/// Test: this test itself.
#[test]
fn the_metadata_table_states_the_audit_scope() {
    let code_only_md = render(true);
    assert!(
        code_only_md.contains("Code-only — repository inspection alone"),
        "{code_only_md}"
    );
    let full_md = render(false);
    assert!(
        full_md.contains("Full — repository inspection plus"),
        "{full_md}"
    );
}

/// Why: with code-only off, the markers are ordinary comments and the report
/// must be exactly what it was before #6669 — no boundary text, no provenance
/// line, and no marker leaking into the page.
/// What: a full-scope render carries none of the three.
/// Test: this test itself.
#[test]
fn a_full_scope_render_carries_no_code_only_text() {
    let md = render(false);
    assert!(!md.contains(code_only::OUT_OF_SCOPE_LEAD), "{md}");
    assert!(!md.contains(code_only::PARTIAL_NOTE), "{md}");
    assert!(
        !md.contains("code_only:"),
        "a marker leaked into output:\n{md}"
    );
}

/// Why (#6004, ported under #6669): these three sections were deferred on the
/// CAST variant. They must render here AND must not be mistaken for CAST
/// health-factor measurements, which is what the trusty-derived disclaimer is
/// for.
/// What: each section is present and the disclaimer says it is not CAST-scored.
/// Test: this test itself.
#[test]
fn the_code_quality_sections_are_present_and_marked_trusty_derived() {
    let md = render(true);
    for heading in [
        "## Code Quality & Architecture",
        "## Security Posture",
        "## Performance & Scalability",
    ] {
        assert!(md.contains(heading), "{heading} missing:\n{md}");
    }
    assert_eq!(
        md.matches("trusty-derived, NOT CAST-scored").count(),
        2,
        "the Code Quality and Security sections each disclaim CAST scoring:\n{md}"
    );
}
