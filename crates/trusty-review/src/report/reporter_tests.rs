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
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build model")
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
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build model")
}

/// Build a model whose single repo's metrics carry TWO RED and TWO AMBER
/// findings — needed to prove per-severity-section sequential numbering
/// (findings-rendering fix, #2357 wave-3.2 defect #1): RED must render 1, 2
/// and AMBER must independently restart at 1, 2 (never continue 3, 4).
fn fixture_model_with_many_findings(dir: &Path) -> ReportModel {
    let metrics = r#"{
      "loc": { "total": 5000, "by_language": [ { "language": "Rust", "loc": 5000 } ] },
      "counts": { "files": 20, "functions": 150 },
      "findings": [
        { "title": "SQL injection", "severity": "red", "category": "security", "component": "db.rs" },
        { "title": "Hardcoded secret", "severity": "red", "category": "security", "component": "auth.rs" },
        { "title": "Stale dependency", "severity": "amber", "category": "maintainability", "component": "deps.toml" },
        { "title": "Weak hashing", "severity": "amber", "category": "security", "component": "hash.rs" }
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
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("build model")
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
    // Metrics-derived per-application fill (values carry a provenance marker).
    assert!(md.contains("5000"));
    assert!(md.contains("20 files, 150 functions"));
    assert!(md.contains("Rust"));
    // #2342.2: self-derived metadata is filled, never honesty-marked.
    assert!(md.contains("trusty-review report (repository inspection) v"));
    // Provenance legend + declared/measured markers are present.
    assert!(md.contains("Provenance:"));
    assert!(md.contains(crate::report::provenance::DECLARED_TAG.trim()));
    // #2342.4 omit-empty: unmapped fields are NOT rendered as marker walls;
    // they are collected into the compact Data gaps list instead.
    assert!(!md.contains(HONESTY_MARKER));
    assert!(md.contains("Data gaps:"));
    // No raw placeholder survives.
    assert!(!md.contains("{{"));
}

/// Why: the CAST template's demonstration `<!-- instruct:executive_summary
/// ... -->` override (#2357 layered instructions) must NEVER leak into a real
/// rendered report — a comment-stripping regression here would be a live
/// prompt-injection-adjacent authoring leak, not just cosmetic noise.
/// What: renders the CAST bundled template through the full pipeline and
/// asserts neither the `instruct:` marker nor its body text appears anywhere
/// in the output, while the report still renders substantively.
/// Test: this test itself.
#[test]
fn cast_template_instruct_override_never_renders() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd-cast")
        .expect("bundled cast template");
    let reporter = Reporter::new(tmp.path());
    let md = reporter.render(&model, &template);

    assert!(!md.contains("instruct:"));
    assert!(!md.contains("TQI (Technical Quality Index) posture"));
    assert!(md.contains("Acme Due Diligence"), "report still renders");
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
/// RED-finding placeholders, tag every synthesized field `inferred` (live-QA
/// wave-2 defect #1), avoid a doubled terminal period where the prose already
/// ends in one (defect #3), and surface the `synthesis: available` note.
/// What: attaches an available `Synthesis` (exec + one RED finding routed to the
/// repo slug) to the model and asserts the prose, the inferred marker, the
/// absence of a double period, and the note appear in the markdown.
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
            evidence_measured: false,
        }],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("A grounded acquirer-relevant summary."));
    assert!(md.contains("Injection risk"));
    assert!(md.contains("Raw query concatenation"));
    assert!(md.contains("synthesis: available"));
    // Defect #1: every synthesized field carries the inferred marker.
    assert!(md.contains(crate::report::provenance::INFERRED_TAG.trim()));
    let inferred_count = md
        .matches(crate::report::provenance::INFERRED_TAG.trim())
        .count();
    // exec summary + description/evidence/business_impact/remediation/cost_effort.
    assert!(
        inferred_count >= 6,
        "expected >=6 inferred tags, got {inferred_count}"
    );
    // Defect #3: the prose's own trailing period was deduped, so the template's
    // literal period never doubles up.
    assert!(!md.contains("concatenation.."));
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
    // Deterministic fallback: the exec summary was never injected — under the
    // omit-empty default the un-synthesised paragraph is dropped (not a marker
    // wall) and recorded under Data gaps.
    assert!(!md.contains("A grounded"));
    assert!(md.contains("Data gaps:"));
    assert!(!md.contains("{{"));
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
    // the RED row's business-impact line falls through to the marker on its
    // own labeled line (findings-rendering fix, #2357 wave-3.2).
    assert!(md.contains(&format!("**Business impact:** {HONESTY_MARKER}")));
    // With no synthesis/investigation evidence at all, the whole evidence block
    // is unset — it falls to the bare honesty-marker paragraph, which the
    // omit-empty pass drops (never rendering a spliced "Evidence: not stated"
    // line); it is recorded under Gaps & Caveats instead.
    assert!(
        !md.contains("Evidence:"),
        "no evidence label without a quote"
    );
    assert!(
        md.contains("Data gaps:"),
        "the dropped evidence is recorded as a gap"
    );
    // Findings are now really-numbered (defect #1 fix), never a literal "N.".
    assert!(md.contains("1. **SQL injection"));
    assert!(!md.contains("N. **"), "literal N. must never render");
    assert!(!md.contains("{{"), "no raw placeholder survives");
}

// ─── Findings-rendering fix (#2357 wave-3.2): numbering, fenced evidence ──────

/// Why: this is the direct regression test for defect #1 — every finding must
/// be a REAL sequential number, and RED/AMBER are independent counters (RED
/// restarts at 1, AMBER restarts at 1; AMBER must never continue from RED).
/// What: a fixture with 2 RED + 2 AMBER findings; asserts "1." and "2." both
/// appear for RED, "1." and "2." both appear for AMBER (not "3."/"4."), and no
/// literal "N." survives anywhere.
/// Test: this test itself.
#[test]
fn finding_numbering_restarts_per_severity_section() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_many_findings(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("1. **SQL injection"), "RED #1");
    assert!(md.contains("2. **Hardcoded secret"), "RED #2");
    assert!(md.contains("1. **Stale dependency"), "AMBER restarts at 1");
    assert!(md.contains("2. **Weak hashing"), "AMBER #2");
    assert!(!md.contains("3. **Stale") && !md.contains("3. **Weak"));
    assert!(
        !md.contains("N. **"),
        "literal N. must never render anywhere"
    );
}

/// Why: this is the direct regression test for defect #2 — evidence must
/// render as a labeled, fenced code block, never spliced inline into prose.
/// What: attaches a verified (measured) evidence quote via synthesis; asserts
/// the file:line label line, the provenance marker, and a ``` fence wrap the
/// verbatim quote — and that the old inline "Evidence: <code>." sentence form
/// never appears.
/// Test: this test itself.
#[test]
fn evidence_renders_as_fenced_block() {
    use crate::report::synthesize::{FindingProse, Synthesis, SynthesisStatus};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.synthesis = Some(Synthesis {
        status: SynthesisStatus::Available,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            app_slug: slug,
            title: "SQL injection".to_string(),
            severity: "RED".to_string(),
            description: "Unsanitised query parameters".to_string(),
            evidence: "let query = `SELECT * FROM users WHERE id = ${id}`;".to_string(),
            component: "lib/auth/session.ts:58".to_string(),
            business_impact: "customer data exposure".to_string(),
            remediation: "use parameterised queries".to_string(),
            cost_effort: "low".to_string(),
            evidence_measured: true,
        }],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // The label line: file:line + measured provenance marker, on its own line.
    assert!(
        md.contains("- **Evidence** (`lib/auth/session.ts:58`)"),
        "evidence label line missing; md:\n{md}"
    );
    assert!(md.contains(crate::report::provenance::MEASURED_TAG.trim()));
    // The quote itself is fenced and verbatim (including its own backtick/`$`).
    assert!(md.contains("```\nlet query = `SELECT * FROM users WHERE id = ${id}`;\n```"));
    // The old inline, unfenced sentence form must never appear again.
    assert!(!md.contains("Evidence: let query"));
    assert!(!md.contains("{{"));
}

/// Why: this is the direct regression test for defect #3 — a blank line
/// inside the evidence quote must never be read as a section boundary and
/// must never splice a "No data available" line mid-quote.
/// What: a verified evidence quote spanning several lines WITH a blank line in
/// the middle; asserts the fenced block contains the blank line literally and
/// the collapse marker never appears inside (or immediately after) it.
/// Test: this test itself.
#[test]
fn evidence_with_blank_line_fences_cleanly() {
    use crate::report::synthesize::{FindingProse, Synthesis, SynthesisStatus};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let quote =
        "function serializeSession(user) {\n\n  return Buffer.from(JSON.stringify(user));\n}";
    model.synthesis = Some(Synthesis {
        status: SynthesisStatus::Available,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            app_slug: slug,
            title: "SQL injection".to_string(),
            severity: "RED".to_string(),
            description: "Session serialization leaks internal state".to_string(),
            evidence: quote.to_string(),
            component: "lib/auth/session.ts:58".to_string(),
            business_impact: "session hijacking".to_string(),
            remediation: "strip internal fields before serializing".to_string(),
            cost_effort: "moderate".to_string(),
            evidence_measured: true,
        }],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    let fenced = format!("```\n{quote}\n```");
    assert!(
        md.contains(&fenced),
        "blank-line evidence must fence with the blank line intact; md:\n{md}"
    );
    // Scope the "no spurious gap splice" check to the RED findings section
    // itself (other, genuinely-empty sections elsewhere in the report
    // legitimately collapse to this same line — that is correct, unrelated
    // behaviour, not the defect under test).
    let red_start = md.find("### 5.1 RED").expect("RED section present");
    let red_end = md.find("### 5.2 AMBER").expect("AMBER section present");
    let red_section = &md[red_start..red_end];
    assert!(
        !red_section.contains("No data available"),
        "no spurious gap splice inside/around the fenced evidence in the RED section; section:\n{red_section}"
    );
}

/// Why: this is the direct regression test for the fence-length rule — a
/// quote that itself contains a ``` sequence must use a LONGER fence so the
/// embedded backticks are never mistaken for the closing fence.
/// What: a verified evidence quote containing a literal ``` run; asserts the
/// wrapping fence is 4+ backticks (strictly longer than the embedded run).
/// Test: this test itself.
#[test]
fn evidence_containing_triple_backticks_uses_longer_fence() {
    use crate::report::synthesize::{FindingProse, Synthesis, SynthesisStatus};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let quote = "const doc = \"```js\\nconsole.log(1)\\n```\";";
    model.synthesis = Some(Synthesis {
        status: SynthesisStatus::Available,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            app_slug: slug,
            title: "SQL injection".to_string(),
            severity: "RED".to_string(),
            description: "Embeds a markdown fence in a string literal".to_string(),
            evidence: quote.to_string(),
            component: "lib/docs.ts:12".to_string(),
            business_impact: "n/a".to_string(),
            remediation: "n/a".to_string(),
            cost_effort: "low".to_string(),
            evidence_measured: true,
        }],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(
        md.contains(&format!("````\n{quote}\n````")),
        "a quote containing ``` must be wrapped in a 4-backtick fence; md:\n{md}"
    );
}

/// Why: the other finding fields must each be their own labeled line, not run
/// together in one paragraph (the readability half of defect #2).
/// What: asserts the Component/Business impact/Remediation labels each appear
/// on their own line, bold-prefixed.
/// Test: this test itself.
#[test]
fn finding_fields_have_own_labeled_lines() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_findings(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("- **Component:**"));
    assert!(md.contains("- **Business impact:**"));
    assert!(md.contains("- **Remediation:**"));
}

/// Why: with no findings metrics, the RED/AMBER/GREEN sections must not be
/// fabricated — under the #2342 omit-empty default they collapse to a single line
/// (no wall of honesty markers) and the empty sections are recorded under gaps.
/// What: renders the base (findings-free) fixture and asserts no marker survives,
/// the collapse line is present, and the gaps list is populated.
/// Test: this test itself.
#[test]
fn reporter_omits_empty_findings_sections_without_metrics() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(!md.contains(HONESTY_MARKER));
    assert!(md.contains("Data gaps:"));
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
            evidence_measured: false,
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
    // Synthesis prose merged onto the same row; the trailing period was deduped
    // (defect #3) so the template's own "." never doubles.
    assert!(md.contains("Unsanitised query parameters"));
    assert!(!md.contains("parameters.."));
    assert!(md.contains("customer data exposure"));
    // Defect #1: the merged prose fields carry the inferred marker.
    assert!(md.contains(crate::report::provenance::INFERRED_TAG.trim()));

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

/// Why: with benchmarking OFF there must be no status note, and under the #2342
/// omit-empty default the unfilled benchmark tables are omitted rather than
/// rendered as a row of honesty markers.
/// What: renders the fixture with `benchmark = None` and asserts no status
/// section and no marker row, and that rendering is deterministic (stable across
/// two renders).
/// Test: this test itself.
#[test]
fn benchmark_off_omits_benchmark_tables() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(!md.contains("## Benchmark Status"));
    let marker_row = format!(
        "| {HONESTY_MARKER} | {HONESTY_MARKER} | {HONESTY_MARKER} | {HONESTY_MARKER} | {HONESTY_MARKER} |"
    );
    assert!(!md.contains(&marker_row));
    assert!(!md.contains("{{"));
    // Deterministic: a second render is byte-identical.
    assert_eq!(md, Reporter::new(tmp.path()).render(&model, &template));
}

// ─── Wave 2 (#2340 / #2342): instructions, self-metadata, scan, provenance ────

/// Why: #2340 requires the analyst brief recorded verbatim in every report.
/// What: builds a model with instructions and asserts the section + verbatim text.
/// Test: this test itself.
#[test]
fn reporter_records_instructions_verbatim() {
    use crate::report::instructions::Instructions;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("acme.json"), "{}").expect("metrics");
    let toml = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"Acme Web\"\npath = \"/nonexistent/x\"\nmetrics = \"acme.json\"\n";
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest");
    let instr = Instructions {
        text: "Focus on auth and data retention.".to_string(),
        source: std::path::PathBuf::from("brief.md"),
    };
    let model = ReportModel::build(
        &manifest,
        &manifest_path,
        "report-technical-dd",
        Some(&instr),
    )
    .expect("model");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(md.contains("## Analyst Instructions"));
    assert!(md.contains("Focus on auth and data retention."));
    assert!(md.contains("provenance: declared"));
}

/// Why: #2342.2 — Section 3 self-describes trusty-review's own scoring model; it
/// must never render as "not stated".
/// What: renders the generic template and asserts the normalized-band text.
/// Test: this test itself.
#[test]
fn reporter_self_describes_scoring_model() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(md.contains("trusty-review normalized 0–100 quality scale"));
    assert!(md.contains("RED < 33"));
}

/// Build a model whose single repo points at a real scanned checkout (no metrics).
fn fixture_model_scanned(dir: &Path, repo: &Path) -> ReportModel {
    std::fs::write(repo.join("main.rs"), "fn main() {\n    run();\n}\n").expect("rs");
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"scanned\"\n\n[dependencies]\nserde = \"1\"\n",
    )
    .expect("cargo");
    let toml = format!(
        "[report]\ntitle = \"Scanned DD\"\n\n[[repositories]]\nname = \"Scanned\"\npath = \"{}\"\n",
        repo.display()
    );
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(&toml, &manifest_path).expect("manifest");
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("model")
}

/// Why: #2342.3 — a bare run against a local repo (no metrics JSON) must produce
/// measured baseline figures directly from the repository.
/// What: renders a scanned repo and asserts the LoC, language, and framework are
/// present and carry the measured provenance marker.
/// Test: this test itself.
#[test]
fn reporter_scans_repo_baseline() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let model = fixture_model_scanned(tmp.path(), &repo);
    assert!(model.repositories[0].scan.is_some(), "scan computed");

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(md.contains("Rust"));
    assert!(md.contains("Cargo.toml: scanned"));
    // Measured provenance marker appears on the scanned values.
    assert!(md.contains(crate::report::provenance::MEASURED_TAG.trim()));
}

/// Why: #2342.3 precedence — an external metrics JSON (declared) wins over the
/// scanned (measured) value where both exist.
/// What: gives a scanned repo an explicit metrics LoC that differs from the scan
/// and asserts the declared figure (and its marker) is what renders.
/// Test: this test itself.
#[test]
fn reporter_declared_metrics_win_over_scanned() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::fs::write(repo.join("main.rs"), "fn main() {}\n").expect("rs");
    std::fs::write(
        tmp.path().join("m.json"),
        r#"{ "loc": { "total": 99999 } }"#,
    )
    .expect("metrics");
    let toml = format!(
        "[report]\ntitle = \"P\"\n\n[[repositories]]\nname = \"R\"\npath = \"{}\"\nmetrics = \"m.json\"\n",
        repo.display()
    );
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(&toml, &manifest_path).expect("manifest");
    let model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("model");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    // Declared LoC wins, tagged declared (not the scanned line count).
    assert!(md.contains(&format!("99999{}", crate::report::provenance::DECLARED_TAG)));
}

// ─── Live-QA wave-2 fixes (#1 inferred tagging, #3 punctuation dedupe, #5) ────

/// Why: live-QA wave-2 defect #3 — a prose field's own trailing period must not
/// double up with a template's literal trailing period.
/// What: asserts the terminal `.` is stripped.
/// Test: this test itself.
#[test]
fn dedupe_strips_trailing_period() {
    use super::dedupe_terminal_punctuation;
    assert_eq!(
        dedupe_terminal_punctuation("Raw query concatenation."),
        "Raw query concatenation"
    );
}

/// Why: a prose field ending in `?` collides with a template's trailing period
/// the same way a `.` does; it must also be deduped.
/// What: asserts the terminal `?` is stripped.
/// Test: this test itself.
#[test]
fn dedupe_strips_trailing_question_mark() {
    use super::dedupe_terminal_punctuation;
    assert_eq!(
        dedupe_terminal_punctuation("Is this exploitable?"),
        "Is this exploitable"
    );
}

/// Why: a trailing `)` has no collision with any template in this crate (no
/// template appends a bare period directly after these fields when they close
/// with a parenthesis) — it must be left untouched, not stripped.
/// What: asserts a trailing `)` survives dedupe unchanged.
/// Test: this test itself.
#[test]
fn dedupe_leaves_trailing_paren() {
    use super::dedupe_terminal_punctuation;
    assert_eq!(
        dedupe_terminal_punctuation("use parameterised queries (see OWASP)"),
        "use parameterised queries (see OWASP)"
    );
}

/// Why: live-QA wave-2 defect #1 — the top-risks table (filled by
/// `inject_synthesis_summary`) must tag its LLM-written fields `inferred`, not
/// just the executive summary.
/// What: attaches a `Synthesis` with one top risk and asserts the description
/// and cost fields carry the inferred marker.
/// Test: this test itself.
#[test]
fn reporter_tags_top_risks_as_inferred() {
    use crate::report::synthesize::{RiskRow, Synthesis, SynthesisStatus};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    model.synthesis = Some(Synthesis {
        status: SynthesisStatus::Available,
        executive_summary: Some("Summary.".to_string()),
        top_risks: vec![RiskRow {
            description: "Unpatched dependency chain".to_string(),
            severity: "RED".to_string(),
            cost: "moderate".to_string(),
            apps: "Acme Web".to_string(),
        }],
        findings: vec![],
        notes: vec![],
    });
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    let inferred = crate::report::provenance::INFERRED_TAG.trim();
    assert!(md.contains("Unpatched dependency chain"));
    assert!(md.contains("moderate"));
    // At least the exec summary + risk description + risk cost carry the marker.
    assert!(md.matches(inferred).count() >= 3);
}

/// Why: live-QA wave-2 defect #5 — the per-language LoC breakdown was computed
/// (both by the scanner and from an external metrics file) but only language
/// NAMES reached the rendered Profile table; the actual split (the useful
/// signal) was dropped.
/// What: builds a model with an explicit multi-language metrics breakdown and
/// asserts the rendered tech-stack cell carries per-language counts, not just
/// names.
/// Test: this test itself.
#[test]
fn reporter_renders_language_breakdown_with_counts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("acme.json"),
        r#"{
          "loc": { "total": 19795, "by_language": [
            { "language": "TypeScript", "loc": 19568 },
            { "language": "SQL", "loc": 184 },
            { "language": "CSS", "loc": 43 }
          ]},
          "counts": { "files": 20, "functions": 150 }
        }"#,
    )
    .expect("write metrics");
    let toml = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"Acme Web\"\npath = \"/nonexistent/x\"\nmetrics = \"acme.json\"\n";
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest");
    let model =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("model");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("TypeScript 19,568"));
    assert!(md.contains("SQL 184"));
    assert!(md.contains("CSS 43"));
    assert!(md.contains(&format!(
        "TypeScript 19,568 · SQL 184 · CSS 43{}",
        crate::report::provenance::DECLARED_TAG
    )));
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
