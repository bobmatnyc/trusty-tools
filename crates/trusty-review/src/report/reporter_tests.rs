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
    // #6004: `complexity` populates Key Facts + Code Quality — every field the
    // bundled template asks for must be present so the full-purity assertions
    // in `render_contains_expected`/`reporter_omits_empty_findings_sections_
    // without_metrics` (zero honesty markers anywhere) still hold.
    let metrics = r#"{
      "loc": { "total": 5000, "by_language": [
        { "language": "Rust", "loc": 5000 }
      ]},
      "counts": { "files": 20, "functions": 150 },
      "complexity": { "buckets": [ { "label": "low (1-5)", "count": 150 } ] }
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
        { "title": "SQL injection", "severity": "red", "category": "authentication & secrets", "component": "db.rs" },
        { "title": "Stale dependency", "severity": "amber", "category": "maintainability", "component": "deps.toml" },
        { "title": "Strong test coverage", "severity": "green", "category": "test coverage", "component": "tests/api.rs:12" },
        { "title": "Clean module boundaries", "severity": "green", "category": "state management", "component": "src/lib.rs:3" },
        { "title": "Constant-time token comparison", "severity": "green", "category": "authentication & secrets", "component": "auth.rs:44" },
        { "title": "Raw SQL string interpolation for PR upsert", "severity": "green", "category": "authentication & secrets", "component": "" },
        { "title": "Atomic redb batch upserts", "severity": "green", "category": "state management", "component": "store.rs:88" }
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

/// Why: #6135 — the report is what makes a wrong model impossible to hide, now
/// that a naming difference resolves instead of stopping the run. The Report
/// Metadata row is where a reader sees it, so it must carry the provider, every
/// role, and both halves of any id the resolver adjusted.
/// What: renders the bundled template with an attribution set, then again with
/// none, and asserts the second states its absence rather than rendering blank.
/// Test: this test itself.
#[test]
fn inference_models_row_states_what_ran() {
    use crate::config::Provider;
    use crate::llm::resolve_model;
    use crate::report::model::{InferenceAttribution, RoleAttribution};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let straight = resolve_model("anthropic/claude-opus-4.8", &Provider::OpenRouter)
        .expect("an agreeing pair resolves");
    let adjusted = resolve_model("bedrock/anthropic/claude-sonnet-4.6", &Provider::OpenRouter)
        .expect("a translatable id resolves");
    model.inference = Some(InferenceAttribution::of(
        "the manifest's [inference] section",
        vec![
            RoleAttribution::of("reviewer", "anthropic/claude-opus-4.8", &straight),
            RoleAttribution::of("verifier", "bedrock/anthropic/claude-sonnet-4.6", &adjusted),
        ],
    ));

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("| Inference models |"), "{md}");
    assert!(md.contains("reviewer: anthropic/claude-opus-4.8"), "{md}");
    assert!(
        md.contains(
            "verifier: bedrock/anthropic/claude-sonnet-4.6 → us.anthropic.claude-sonnet-4-6"
        ),
        "an adjusted id renders as requested → ran: {md}"
    );
    assert!(md.contains("the manifest's [inference] section"), "{md}");

    // A model with no resolved selection states that, rather than leaving a row
    // a reader would take for "no inference was involved".
    let bare = Reporter::new(tmp.path()).render(&fixture_model(tmp.path()), &template);
    assert!(bare.contains("not recorded"), "{bare}");
}

/// (a) #6004: a model WITH analyze data yields both the Code Quality &
/// Architecture and Security Posture sections populated — never blank
/// scaffolding.
/// Test: this test itself.
#[test]
fn code_quality_and_security_sections_populate_from_analyze_data() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_findings(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("## Code Quality & Architecture"));
    assert!(md.contains("## Security Posture"));
    assert!(md.contains("## Performance & Scalability"));
    // Code Quality: the fixture's LoC/language/maintainability-finding data.
    assert!(md.contains("Acme Web"));
    assert!(md.contains("5000"));
    // Security Posture (#6137): the fixture's RED "authentication & secrets"
    // finding, and nothing from the other dimensions.
    let security_section = md
        .split("## Security Posture")
        .nth(1)
        .and_then(|s| s.split("## Performance").next())
        .expect("Security Posture section present");
    assert!(!security_section.contains("_No data available"));
    assert!(
        security_section.contains("authentication & secrets"),
        "section: {security_section}"
    );
    assert!(
        !security_section.contains("maintainability"),
        "section: {security_section}"
    );
    assert!(
        security_section.contains("Constant-time token comparison (`auth.rs:44`)"),
        "the dimension's clean signals are credited with their citation: {security_section}"
    );
    // #6080: the fixture's uncited GREEN describes a defect. It reached the
    // green bucket, and the citation requirement is what keeps it out of the
    // clean-signals list where a reader could not check it.
    assert!(
        !security_section.contains("Raw SQL string interpolation"),
        "an uncited GREEN must not be credited as a clean signal: {security_section}"
    );
    // Performance stays the fixed gap text regardless of the data present.
    assert!(md.contains(crate::report::reporter_performance::PERFORMANCE_NOTE));
}

/// #6137: every GREEN finding renders as its own topic line. The templates
/// carried exactly three fixed `{{green_topic_N}}` slots, so a run with 21
/// GREEN findings silently dropped 18 of them.
#[test]
fn reporter_renders_every_green_topic() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_findings(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    for title in [
        "Strong test coverage",
        "Clean module boundaries",
        "Constant-time token comparison",
        "Atomic redb batch upserts",
    ] {
        assert!(
            md.contains(&format!("- {title}")),
            "every GREEN topic renders, including beyond the old three-slot cap: {title}"
        );
    }
}

/// The GREEN section carries titles ONLY — the no-green-analysis rule bans
/// elaboration, and making the bullets repeatable must not smuggle any in.
#[test]
fn reporter_fills_green_topics() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_findings(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    let green_section = md
        .split("### 5.3 GREEN")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("GREEN section present");
    assert!(green_section.contains("- Strong test coverage"));
    assert!(
        !green_section.contains("Remediation"),
        "no elaboration: {green_section}"
    );
    assert!(
        !green_section.contains(HONESTY_MARKER),
        "no unfilled slot survives: {green_section}"
    );
}

/// #6080: every GREEN bullet names the file it was read from.
///
/// Why: 0 of 23 GREEN topics in one report carried a file, a line, or a quote,
/// and Security Posture then cited five of them as clean signals. A citation
/// is not elaboration — it says where to look, which is what lets a reader
/// check a claimed strength at all.
/// What: a cited GREEN renders `title — \`file:line\``; an uncited one renders
/// the bare title, and the citation stays a single line with no prose.
#[test]
fn green_topic_carries_its_citation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_findings(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    let green_section = md
        .split("### 5.3 GREEN")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("GREEN section present");
    assert!(
        green_section.contains("- Constant-time token comparison — `auth.rs:44`"),
        "green bullets carry their citation: {green_section}"
    );
    assert!(
        green_section.contains("- Raw SQL string interpolation for PR upsert\n"),
        "an uncited green renders bare: {green_section}"
    );
}

/// (b) #6004: a model WITHOUT analyze data leaves both sections as honesty
/// markers folded into Gaps & Caveats, never blank or fabricated — Code
/// Quality's table collapses (no metrics anywhere) and Performance still
/// states its fixed gap text (never a silent absence).
/// Test: this test itself.
#[test]
fn code_quality_and_security_sections_gap_without_analyze_data() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let toml = r#"
        [report]
        title = "No Metrics DD"

        [[repositories]]
        name = "Acme Web"
        remote = "acme/web"
    "#;
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("build model");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("## Code Quality & Architecture"));
    assert!(md.contains("## Security Posture"));
    assert!(
        !md.contains(HONESTY_MARKER),
        "omit-empty must fold every unmapped field into Gaps, never leave a marker: {md}"
    );
    assert!(md.contains("Data gaps:"));
    // Performance & Scalability is FIXED — present and identical even with
    // zero analyze data (never a silent gap, never LLM-touched).
    assert!(md.contains(crate::report::reporter_performance::PERFORMANCE_NOTE));
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
/// Since #6046 the bundled template also yields the authorship document, so the
/// count is three — `write_emits_the_authorship_document_alongside` pins its
/// name and ordering.
/// Test: this test itself.
#[test]
fn write_emits_both() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    // #5454: `write` requires a completed synthesis pass; an empty one is the
    // minimum this test needs, since what it asserts is the file pair.
    model.synthesis = Some(crate::report::synthesize::Synthesis::default());
    let out_dir = tmp.path().join("out");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let reporter = Reporter::new(&out_dir);
    let written = reporter.write(&model, &template).expect("write ok");
    assert_eq!(written.len(), 3, "{written:?}");

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
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: Some("A grounded acquirer-relevant summary.".to_string()),
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: String::new(),
            app_slug: slug,
            title: "Injection risk".to_string(),
            severity: "RED".to_string(),
            description: "Raw query concatenation.".to_string(),
            evidence: "one path".to_string(),
            // #6082: a component must name a file — a bare topic word is the
            // shape `is_self_restatement` now suppresses.
            component: "lib/auth/session.ts:58".to_string(),
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

/// Why: a synthesis whose narrative fields were ALL rejected by the numeric
/// guardrail still renders — a rejection is per-field, not a failed pass — and
/// the report must name each rejection so a reader can tell a
/// deterministically-composed section from one the model wrote. #5454 replaced
/// this test's former subject (`Synthesis::unavailable`), which no longer exists:
/// a failed pass never reaches the reporter at all.
/// What: attaches a `Synthesis` carrying only guardrail notes; asserts the notes
/// render and the exec-summary placeholder falls through.
/// Test: this test itself.
#[test]
fn reporter_appends_guardrail_rejection_note() {
    use crate::report::synthesize::Synthesis;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        notes: vec![
            "synthesis: rejected (unverified figure) in executive summary: 9999".to_string(),
        ],
        ..Default::default()
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("synthesis: available"));
    assert!(md.contains("rejected (unverified figure) in executive summary: 9999"));
    // The rejected exec summary was never injected — under the omit-empty default
    // the un-synthesised paragraph is dropped (not a marker wall) and recorded
    // under Data gaps.
    assert!(!md.contains("A grounded"));
    assert!(md.contains("Data gaps:"));
    assert!(!md.contains("{{"));
}

/// Why: #5454 — inference is required, so nothing may reach DISK without a
/// synthesis pass behind it. `render` stays infallible for unit tests of the
/// deterministic composition; `write` is the boundary that enforces the rule.
/// What: hands `write` a model whose `synthesis` is `None`; asserts it refuses
/// and that neither output file was created.
/// Test: this test itself.
#[test]
fn write_refuses_a_model_with_no_synthesis() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    assert!(model.synthesis.is_none(), "fixture must be synthesis-free");

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let out = tmp.path().join("reports");
    let err = Reporter::new(&out)
        .write(&model, &template)
        .expect_err("a synthesis-free model must not be written");
    assert!(
        matches!(err, crate::report::ReportError::SynthesisRequired),
        "expected SynthesisRequired, got {err:?}"
    );
    assert!(
        !out.join("2026-07-10-acme-technical-dd.md").exists(),
        "no markdown may be written for a refused model"
    );
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

// ─── Self-restating findings are suppressed (#6082) ──────────────────────────

/// Render the bundled template with one synthesis finding, and return the
/// markdown — the shared body of the self-restatement tests below.
fn render_with_finding(f: crate::report::synthesize::FindingProse) -> String {
    use crate::report::synthesize::Synthesis;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let mut f = f;
    f.app_slug = slug;
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![f],
        notes: vec![],
    });
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    Reporter::new(tmp.path()).render(&model, &template)
}

/// A finding shaped like the dogfood report's #156/#157, with the fields the
/// individual tests vary left to the caller.
fn restating_finding() -> crate::report::synthesize::FindingProse {
    crate::report::synthesize::FindingProse {
        trace_verdict: String::new(),
        app_slug: String::new(),
        title: "Extract method — hnsw_store".to_string(),
        severity: "AMBER".to_string(),
        description: "The HNSW store module is flagged for method extraction".to_string(),
        evidence: "crates/trusty-common/src/memory_core/store/hnsw_store.rs (extract method)"
            .to_string(),
        component: "trusty-common memory_core".to_string(),
        business_impact: "harder to change".to_string(),
        remediation: "decompose the flagged functions".to_string(),
        cost_effort: "moderate".to_string(),
        evidence_measured: false,
    }
}

/// #6082: findings #156 and #157 of the dogfood report were the model
/// re-reporting analyze findings #1 and #3. Their Evidence block quoted the
/// finding's own LABEL instead of a line of source, and their Component was a
/// prose topic instead of a path — the shape no genuine finding has.
#[test]
fn a_self_quoting_finding_is_suppressed() {
    let mut f = restating_finding();
    // A real path, so only the self-quote can be what suppresses it.
    f.component = "crates/trusty-common/src/memory_core/store/hnsw_store.rs".to_string();

    // Scoped to the report body: since #6082 lap 6 a refused narrative is
    // DISCLOSED under Synthesis Status, so its title appears in the document by
    // design. What must not survive is the rendered finding.
    let body = report_body(&render_with_finding(f));

    assert!(
        !body.contains("Extract method — hnsw_store"),
        "a finding quoting its own label must not survive:\n{body}"
    );
}

/// A Component naming a topic rather than a file is the other half of the same
/// shape — the reader is given nothing to open.
#[test]
fn a_finding_with_a_non_path_component_is_suppressed() {
    let body = report_body(&render_with_finding(restating_finding()));

    assert!(
        !body.contains("Extract method — hnsw_store"),
        "a finding citing a topic instead of a file must not survive:\n{body}"
    );
}

/// The rendered document without its appended Synthesis Status list.
///
/// A refused narrative is disclosed there by title (#6082 lap 6), so a
/// document-wide `!contains(title)` assertion can no longer tell a suppressed
/// finding from a rendered one.
fn report_body(md: &str) -> String {
    md.split("## Synthesis Status")
        .next()
        .unwrap_or(md)
        .to_string()
}

/// The filter must not reach a genuine finding: a real path plus a quote of real
/// source, which is what every legitimate finding in the graded report looked
/// like (e.g. #155, `const MEM_CEILING_MB: u64 = 8 * 1024;`).
#[test]
fn a_genuine_finding_survives_the_self_quote_filter() {
    let mut f = restating_finding();
    f.title = "Hardcoded illustrative resource ceilings for gauges".to_string();
    f.component = "crates/trusty-mpm/src/tui/health/screen.rs:384".to_string();
    f.evidence = "const MEM_CEILING_MB: u64 = 8 * 1024;".to_string();

    let amber = amber_section(&render_with_finding(f)).to_string();

    assert!(
        amber.contains("Hardcoded illustrative resource ceilings"),
        "a genuine finding must survive:\n{amber}"
    );
}

/// A root-level manifest citation (`Cargo.toml`, `deny.toml`) has neither a
/// directory nor a line number and must still read as a file, not a topic.
#[test]
fn a_root_level_manifest_component_is_not_a_topic() {
    let mut f = restating_finding();
    f.title = "Unpinned caret-range AWS SDK dependencies".to_string();
    f.component = "Cargo.toml".to_string();
    f.evidence = "aws-config = \"^1\"".to_string();

    let amber = amber_section(&render_with_finding(f)).to_string();

    assert!(
        amber.contains("Unpinned caret-range AWS SDK dependencies"),
        "Cargo.toml is a real citation:\n{amber}"
    );
}

/// #6082: an AMBER finding's business impact exists for every red/amber finding
/// the investigation produces (138 of 138 in the dogfood run) and rendered only
/// for the 3 REDs — the template had no slot for it in the amber block.
#[test]
fn an_amber_finding_renders_its_business_impact() {
    let mut f = restating_finding();
    f.title = "Cross-process ledger writes can silently lose updates".to_string();
    f.component = "crates/trusty-agents-common/src/workstreams/ledger.rs:33".to_string();
    f.evidence = "let mut ledger = read_ledger()?;".to_string();
    f.business_impact = "concurrent sessions silently drop workstream state".to_string();

    let md = render_with_finding(f);

    assert!(
        md.contains("concurrent sessions silently drop workstream state"),
        "an AMBER finding must render its business impact:\n{md}"
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
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: String::new(),
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
    // #6166 leg 2: a finding the verdict pass never reached carries no marker.
    assert!(!md.contains("- **Trace:**"), "md:\n{md}");
}

/// Why (#6166 leg 2): the verdict has to reach the page next to the finding it
/// judged, and it must sit OUTSIDE the fence — the quote inside is the
/// byte-for-byte text the evidence guardrail matched, and a line spliced into
/// it would make the displayed quote diverge from the verified one.
/// What: a `trace_verdict` renders as one `- **Trace:**` bullet after the
/// closing fence; the fenced quote is unchanged.
/// Test: this test itself.
#[test]
fn a_trace_verdict_renders_under_the_evidence_fence() {
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: "cleared-by-trace: the query is parameterised at line 61".to_string(),
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

    assert!(
        md.contains("```\n- **Trace:** cleared-by-trace: the query is parameterised at line 61"),
        "the verdict must follow the CLOSING fence; md:\n{md}"
    );
    assert!(
        md.contains("```\nlet query = `SELECT * FROM users WHERE id = ${id}`;\n```"),
        "the verified quote must be untouched; md:\n{md}"
    );
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
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let quote =
        "function serializeSession(user) {\n\n  return Buffer.from(JSON.stringify(user));\n}";
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: String::new(),
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
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let quote = "const doc = \"```js\\nconsole.log(1)\\n```\";";
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: String::new(),
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

/// Count occurrences of "No data available" strictly BETWEEN a ``` open and
/// its matching close, anywhere in `md` — the full-render regression assertion
/// requested for the evidence-splice defect (#2357 wave-3.2 follow-up): this
/// must always be zero, regardless of which pass could theoretically inject a
/// gap-collapse line.
fn count_marker_inside_fences(md: &str) -> usize {
    let mut in_fence = false;
    let mut hits = 0usize;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence && line.contains("No data available") {
            hits += 1;
        }
    }
    hits
}

/// Why: this is the DIRECT regression test for the real root cause the
/// coordinator's QA pinned — not a blank line, but multiple ADJACENT
/// `#`-prefixed comment lines (as in a real `.env.example`) with NO blank
/// lines between them.  `collapse_recursive`'s heading-span lookahead scan
/// (the inner loop that finds where a heading's body ends) was not
/// fence-aware, so it misread each `#`-comment line inside the fence as a
/// terminating markdown heading of its own — collapsing each one's (empty)
/// body to a spurious "No data available" line. This exact scenario
/// (evidence = `# ALLOWED_OAUTH_DOMAINS: ...` / `# claim) ...` / `# is
/// empty.`) is the literal text QA extracted from the JSON twin's
/// `synthesis.findings[12].evidence` in the repro.
/// What: attaches that verbatim evidence via synthesis; asserts the rendered
/// fenced block is byte-identical to the source (`\n`-joined, no insertions),
/// zero "No data available" occurrences inside any fence anywhere in the
/// document, and the gaps list never contains a fragment of the evidence text.
/// Test: this test itself.
#[test]
fn evidence_with_adjacent_hash_comment_lines_renders_byte_identical() {
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let quote = "# ALLOWED_OAUTH_DOMAINS: comma-separated Google Workspace hosted-domains (`hd`\n# claim) allowed to enter the visualization area. The gate FAILS CLOSED if this\n# is empty.";
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: String::new(),
            app_slug: slug,
            title: "ALLOWED_OAUTH_DOMAINS fails closed but is undocumented".to_string(),
            severity: "RED".to_string(),
            description: "documented to fail closed when empty".to_string(),
            evidence: quote.to_string(),
            component: ".env.example:85".to_string(),
            business_impact: "silent lockout".to_string(),
            remediation: "add a startup assertion".to_string(),
            cost_effort: "low".to_string(),
            evidence_measured: true,
        }],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // Byte-identical: the exact `\n`-joined source, fenced, nothing inserted.
    let fenced = format!("```\n{quote}\n```");
    assert!(
        md.contains(&fenced),
        "evidence must render byte-identical inside its fence; md:\n{md}"
    );
    // The full-render grep assertion: zero splices inside ANY fence anywhere.
    assert_eq!(
        count_marker_inside_fences(&md),
        0,
        "no 'No data available' may ever appear inside a fenced block; md:\n{md}"
    );
    // The corrupted fragments must never leak into the Gaps & Caveats list.
    let gaps_start = md
        // #5405 renumbered this to §9; §8 is now Ticketing & Delivery
        // Traceability.
        .find("## 9. Gaps & Caveats")
        .expect("gaps section present");
    let gaps_section = &md[gaps_start..];
    assert!(
        !gaps_section.contains("ALLOWED_OAUTH_DOMAINS"),
        "evidence fragments must never leak into Data gaps; gaps:\n{gaps_section}"
    );
    assert!(!gaps_section.contains("claim) allowed"));
}

/// Why: a genuine blank line inside the evidence (as distinct from adjacent
/// `#`-comments with none) must still preserve literally — regression net for
/// both classes of the same defect via the shared fence-aware collapse fix.
/// What: reuses the blank-line fixture and asserts zero marker occurrences
/// inside any fence via the same full-render grep helper.
/// Test: this test itself.
#[test]
fn evidence_with_blank_line_full_render_has_no_fenced_splice() {
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let quote = "AI_GATEWAY_API_KEY=\n\n# Optional overrides for the gateway model IDs (format: \"provider/model\").\n# Defaults: AI_GATEWAY_MODEL=anthropic/claude-sonnet-4-5-20250929,";
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: String::new(),
            app_slug: slug,
            title: "AI Gateway secrets".to_string(),
            severity: "RED".to_string(),
            description: "no rotation guidance".to_string(),
            evidence: quote.to_string(),
            component: ".env.example:53".to_string(),
            business_impact: "credential exposure".to_string(),
            remediation: "use a secrets manager".to_string(),
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
        "blank-line evidence must render byte-identical inside its fence; md:\n{md}"
    );
    assert_eq!(count_marker_inside_fences(&md), 0);
}

/// Why: this is the direct regression test for defect #4 — the per-application
/// scorecard heading `### 4.N. {{app_name}}` was never substituted; every
/// application must get a real 1-based sub-index.
/// What: a two-repository model; asserts `4.1.` and `4.2.` both render and
/// the literal `4.N.` never appears.
/// Test: this test itself.
#[test]
fn scorecard_heading_renders_real_index() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("acme.json"), r#"{"loc": {"total": 100}}"#)
        .expect("write metrics");
    let toml = r#"
        [report]
        title = "Multi-App DD"

        [[repositories]]
        name = "App One"
        path = "/nonexistent/one"
        metrics = "acme.json"

        [[repositories]]
        name = "App Two"
        path = "/nonexistent/two"
    "#;
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("build model");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("### 4.1. App One"));
    assert!(md.contains("### 4.2. App Two"));
    assert!(!md.contains("4.N."), "literal 4.N. must never render");
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

/// Why (#5317): every analyze-derived finding rendered its description and
/// remediation as `not stated in source data` even when the daemon had returned
/// both. Those two are deterministic tool output — the row must state them
/// without waiting for synthesis.
/// What: renders a metrics fixture carrying `description` and `remediation` and
/// asserts both reach the page, and that neither slot falls to the marker.
/// Test: this test itself.
#[test]
fn reporter_renders_metric_description_and_remediation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let metrics = r#"{
      "findings": [
        { "title": "Extract method", "severity": "amber", "category": "maintainability",
          "component": "src/hiargs.rs",
          "description": "cyclomatic complexity 31 (grade F)",
          "remediation": "Extract the body of 'from_low_args' into 2-3 smaller functions" }
      ]
    }"#;
    std::fs::write(tmp.path().join("acme.json"), metrics).expect("write metrics");
    let toml = r#"
        [report]
        title = "Acme Due Diligence"

        [[repositories]]
        name = "Acme Web"
        path = "/nonexistent/acme-web"
        metrics = "acme.json"
    "#;
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("build model");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(
        md.contains("cyclomatic complexity 31 (grade F)"),
        "the tool's own observation must render: {md}"
    );
    assert!(
        md.contains("Extract the body of 'from_low_args' into 2-3 smaller functions"),
        "the tool's own suggested action must render: {md}"
    );
    assert!(
        !md.contains(&format!("**Remediation:** {HONESTY_MARKER}")),
        "remediation must not fall to the honesty marker when the source stated it: {md}"
    );
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
    use crate::report::synthesize::{FindingProse, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model_with_findings(tmp.path());
    let slug = model.repositories[0].slug.clone();
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings: vec![FindingProse {
            trace_verdict: String::new(),
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
    // expanded with real data — the real section 4 heading appears exactly
    // once, with its real 1-based index substituted (never the literal `4.N.`).
    assert_eq!(md.matches("### 4.1. Acme Web").count(), 1);
    assert!(!md.contains("4.N."), "literal 4.N. must never render");
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

/// Why: #6180 — instructions can now arrive with nothing declaring them, so the
/// page must name the file that extended the auditor prompt and state that the
/// deterministic checks still ran. A reader who sees only the brief cannot tell
/// whether it overrode the guards.
/// What: renders a model whose instructions came from a discovered
/// `instructions.md` and asserts the note names the file and the guards; then
/// asserts a model with no instructions renders no such note at all.
/// Test: this test itself.
#[test]
fn the_instructions_note_names_the_file_and_the_guards() {
    use crate::report::instructions::Instructions;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("acme.json"), "{}").expect("metrics");
    let toml = "[report]\ntitle = \"Acme\"\n\n[[repositories]]\nname = \"Acme Web\"\npath = \"/nonexistent/x\"\nmetrics = \"acme.json\"\n";
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest");
    let instr = Instructions {
        text: "Weigh secrets handling above all.".to_string(),
        source: tmp.path().join("instructions.md"),
    };
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");

    let model = ReportModel::build(
        &manifest,
        &manifest_path,
        "report-technical-dd",
        Some(&instr),
    )
    .expect("model");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    assert!(md.contains("Custom auditor instructions from"), "{md}");
    assert!(md.contains("instructions.md"), "{md}");
    assert!(md.contains("EXTEND the auditor prompt"), "{md}");
    assert!(
        md.contains("deterministic post-synthesis checks"),
        "the note must say the guards still ran: {md}"
    );

    // Absent instructions render nothing — the no-instructions page is unchanged.
    let bare =
        ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("model");
    let bare_md = Reporter::new(tmp.path()).render(&bare, &template);
    assert!(
        !bare_md.contains("Custom auditor instructions"),
        "{bare_md}"
    );
    assert!(!bare_md.contains("## Analyst Instructions"), "{bare_md}");
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
    use super::super::reporter_findings::dedupe_terminal_punctuation;
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
    use super::super::reporter_findings::dedupe_terminal_punctuation;
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
    use super::super::reporter_findings::dedupe_terminal_punctuation;
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
    use crate::report::synthesize::{RiskRow, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
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

/// Why: #2373 — the Top Risks table hard-capped at 3 fixed placeholder rows, so
/// synthesized rows 4-5 were silently dropped even though the JSON twin carried
/// them and the synthesis schema caps `top_risks` at 5. The repeatable
/// `top_risk_row` block must render EVERY synthesized row, in order, with
/// sequential 1..N ranks and the `⁽ⁱ⁾` provenance tag intact on the prose cells.
/// What: attaches a `Synthesis` carrying the full 5 top-risk rows and asserts all
/// five descriptions render (rows 4 and 5 explicitly), the ranks number 1..5, and
/// the inferred marker survives on the synthesized prose.
/// Test: this test itself.
#[test]
fn reporter_renders_all_top_risk_rows() {
    use crate::report::synthesize::{RiskRow, Synthesis};

    let risk = |n: usize| RiskRow {
        description: format!("Risk number {n} description"),
        severity: "RED".to_string(),
        cost: format!("cost-{n}"),
        apps: format!("App {n}"),
    };

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: Some("Summary.".to_string()),
        top_risks: (1..=5).map(risk).collect(),
        findings: vec![],
        notes: vec![],
    });
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // All five rows render — rows 4 and 5 were the ones previously dropped.
    for n in 1..=5 {
        assert!(
            md.contains(&format!("Risk number {n} description")),
            "row {n} missing from rendered markdown"
        );
    }
    // Ranks number sequentially 1..5 within the Top Risks table rows.
    for n in 1..=5 {
        assert!(
            md.contains(&format!("| {n} | Risk number {n} description")),
            "row {n} not ranked sequentially"
        );
    }
    // The ⁽ⁱ⁾ provenance tag survives on the synthesized prose cells.
    let inferred = crate::report::provenance::INFERRED_TAG.trim();
    assert!(md.contains(inferred));
    // description + cost per row (5 rows) plus the exec summary → >= 11 markers.
    assert!(
        md.matches(inferred).count() >= 11,
        "expected >=11 inferred tags across 5 risk rows, got {}",
        md.matches(inferred).count()
    );
}

/// Why: #2373 omit-empty — with zero synthesized top risks (deterministic-only
/// run, or synthesis absent), no `top_risk_row` block is pushed, so the table
/// must collapse per the existing rules rather than leaving an empty header +
/// separator skeleton behind.
/// What: renders a model whose synthesis carries no top risks and asserts the
/// Top Risks table header row is absent from the polished markdown (the section
/// collapsed) while no raw placeholder leaks through.
/// Test: this test itself.
#[test]
fn reporter_collapses_empty_top_risks() {
    use crate::report::synthesize::Synthesis;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: Some("Summary.".to_string()),
        top_risks: vec![],
        findings: vec![],
        notes: vec![],
    });
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // No empty table skeleton: the Top Risks header row is collapsed away.
    assert!(
        !md.contains("| # | Risk | Severity | Est. cost/effort |"),
        "empty Top Risks table skeleton should have collapsed"
    );
    // The unfilled per-row placeholders never leak as literal text.
    assert!(!md.contains("{{risk_description}}"));
    assert!(!md.contains("{{risk_rank}}"));
}

/// Why: #6009 shape 2 — a live capture's `top_risks` rows omitted
/// `severity`/`cost` entirely, and `RiskRow` now defaults each to `""`
/// (`synthesize.rs`) rather than failing the whole response. The reporter
/// must render that honestly — `not stated in source data` — never a blank
/// cell (which could read as "no severity") and never an invented band or
/// figure.
/// What: attaches a `Synthesis` with one top-risk row carrying real
/// `description`/`apps` but empty `severity`/`cost`, and asserts the rendered
/// row's severity/cost cells carry [`HONESTY_MARKER`] while the real fields
/// still render.
/// Test: this test itself.
#[test]
fn reporter_renders_defaulted_top_risk_severity_honestly() {
    use crate::report::synthesize::{RiskRow, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    model.synthesis = Some(Synthesis {
        executive_summary: Some("Summary.".to_string()),
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        top_risks: vec![RiskRow {
            description: "Plaintext secrets at rest".to_string(),
            severity: String::new(),
            cost: String::new(),
            apps: "00-bobmatnyc-trusty-tools".to_string(),
        }],
        findings: vec![],
        notes: vec![],
    });
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("Plaintext secrets at rest"));
    assert!(md.contains("00-bobmatnyc-trusty-tools"));
    assert!(
        md.contains(&format!("| 1 | Plaintext secrets at rest ⁽ⁱ⁾ | {HONESTY_MARKER} | {HONESTY_MARKER} | 00-bobmatnyc-trusty-tools |")),
        "a defaulted severity/cost must render as the honesty marker, never blank or fabricated: {md}"
    );
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

/// A minimal template carrying one POPULATED dataset table (literal values, so
/// it survives fill + omit-empty) to exercise the wave-4 mermaid injection.
const MERMAID_TEMPLATE: &str = "# Report\n\n\
    ## 7. Graph-Ready Data Appendix\n\n\
    <!-- dataset: loc_by_tech | chart: bar | x: tech | y: loc -->\n\
    | Tech | LoC |\n|---|---|\n| Rust | 8200 |\n| Python | 3100 |\n";

/// Why: when mermaid is enabled (the default) the reporter must emit a ```mermaid
/// block directly under a populated §7 dataset table (#2366).
/// What: renders a template with one literal-value dataset table and asserts the
/// `xychart-beta` block appears AFTER the table.
/// Test: this test itself.
#[test]
fn render_injects_mermaid_under_populated_dataset() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let md = Reporter::new(tmp.path()).render(&model, MERMAID_TEMPLATE);
    assert!(md.contains("```mermaid"), "block emitted:\n{md}");
    assert!(md.contains("xychart-beta"));
    assert!(md.contains("x-axis [\"Rust\", \"Python\"]"), "{md}");
    assert!(md.contains("bar [8200, 3100]"), "{md}");
    let table_pos = md.find("| Python |").unwrap();
    let block_pos = md.find("```mermaid").unwrap();
    assert!(table_pos < block_pos, "chart follows table");
}

/// Why: `--no-mermaid` / `[report] mermaid = false` must disable charts entirely,
/// leaving output byte-identical to the pre-wave-4 report (#2366).
/// What: renders the same model+template with `with_mermaid(false)`; asserts no
/// mermaid artifacts appear and the pipe table is untouched.
/// Test: this test itself.
#[test]
fn no_mermaid_byte_identical() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let off = Reporter::new(tmp.path())
        .with_mermaid(false)
        .render(&model, MERMAID_TEMPLATE);
    let on = Reporter::new(tmp.path()).render(&model, MERMAID_TEMPLATE);
    assert!(
        !off.contains("```mermaid"),
        "no chart when disabled:\n{off}"
    );
    assert!(!off.contains("xychart-beta"));
    assert!(on.contains("```mermaid"), "chart when enabled");
    // Disabling is purely additive-off: the `on` output is the `off` output with
    // the chart block inserted, so removing every mermaid fence region recovers it.
    assert!(on.len() > off.len());
    assert!(off.contains("| Rust | 8200 |"), "table itself untouched");
}

// ─── §7 graph-appendix repo-derivable dataset fill (#2366 follow-up) ───────

/// Initialise a tiny git repo at `dir` with Rust + TypeScript sources so
/// `scan_repo` computes a real, non-empty per-language LoC breakdown (mirrors
/// `scan_tests.rs::git_init_add`).
fn git_repo_with_languages(dir: &Path) {
    std::fs::write(
        dir.join("main.rs"),
        "fn main() {\n    work();\n    work();\n}\n",
    )
    .expect("write rs");
    std::fs::write(
        dir.join("app.ts"),
        "const x = 1;\nexport {};\nconst y = 2;\n",
    )
    .expect("write ts");
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("init")
        .output();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["add", "-A"])
        .output();
}

/// Why: live-QA on a real bare run (no `--synthesize`, no external metrics JSON)
/// found the §7 `loc_by_technology` dataset table NEVER populated — it had no
/// fill path, so it collapsed under omit-empty and the Mermaid injector had
/// nothing to chart. This dataset's data (per-language LoC) is exactly what the
/// built-in scan already computes.
/// What: builds a model from a manifest pointing at a REAL scanned checkout (no
/// `metrics` key) using the bundled `report-technical-dd` template, and asserts
/// the dataset table renders with the scanned languages under `measured`
/// provenance, and a real `xychart-beta` chart follows it.
/// Test: this test itself.
#[test]
fn bare_scan_populates_loc_by_technology() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).expect("mkdir");
    git_repo_with_languages(&repo_dir);

    let manifest_toml = format!(
        "[report]\ntitle = \"Bare Scan DD\"\n\n[[repositories]]\nname = \"Acme\"\npath = \"{}\"\n",
        repo_dir.display()
    );
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(&manifest_toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");
    // Sanity: the scan actually ran and found languages (else this test proves nothing).
    let scan = model.repositories[0].scan.as_ref().expect("scan present");
    assert!(!scan.by_language.is_empty(), "scan found languages");

    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("Rust"), "loc_by_technology row for Rust:\n{md}");
    assert!(
        md.contains("TypeScript"),
        "loc_by_technology row for TS:\n{md}"
    );
    // Measured provenance: no metrics JSON was supplied, only the scan.
    assert!(
        md.contains('⁽'),
        "measured/declared provenance tag present:\n{md}"
    );
    // A real mermaid chart renders under the now-populated dataset.
    assert!(
        md.contains("```mermaid"),
        "chart emitted for scan-only run:\n{md}"
    );
    assert!(md.contains("xychart-beta"));
}

/// Why: an external trusty-analyze metrics JSON is ENRICHMENT that must win over
/// the measured scan (mirrors `fill_profile`'s existing precedence) — a stale or
/// differing scan number must never leak into the dataset table when a more
/// authoritative declared figure exists.
/// What: a repo with BOTH a real scanned checkout AND a metrics JSON declaring a
/// different LoC for the same language; asserts the DECLARED number renders
/// (under its provenance tag), not the scanned one.
/// Test: this test itself.
#[test]
fn declared_metrics_win_for_loc_by_technology() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).expect("mkdir");
    git_repo_with_languages(&repo_dir); // scan would find a handful of Rust lines

    std::fs::write(
        tmp.path().join("acme.json"),
        r#"{ "loc": { "total": 9000, "by_language": [
          { "language": "Rust", "loc": 9000 }
        ]}}"#,
    )
    .expect("write metrics");

    let manifest_toml = format!(
        "[report]\ntitle = \"Declared Wins\"\n\n[[repositories]]\nname = \"Acme\"\npath = \"{}\"\nmetrics = \"acme.json\"\n",
        repo_dir.display()
    );
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(&manifest_toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");

    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("9,000"), "declared LoC wins over scan:\n{md}");
    assert!(
        md.contains(&format!("9,000{}", crate::report::provenance::DECLARED_TAG)),
        "declared provenance tag on the declared value:\n{md}"
    );
}

/// Why: cyclomatic-complexity buckets are NOT computable by the built-in scan
/// (`RepoScan` has no complexity analysis) — they exist only in an externally
/// supplied trusty-analyze metrics JSON. When that data IS present, wiring must
/// fill the dataset from it (never fabricate).
/// What: a repo whose metrics declare two complexity buckets; asserts both
/// bucket labels/percentages render and a `xychart-beta` bar chart follows.
/// Test: this test itself.
#[test]
fn complexity_distribution_fills_from_metrics() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("acme.json"),
        r#"{ "complexity": { "buckets": [
          { "label": "low (1-5)", "count": 80 },
          { "label": "high (>20)", "count": 20 }
        ]}}"#,
    )
    .expect("write metrics");
    let manifest_toml = "[report]\ntitle = \"Complexity\"\n\n[[repositories]]\nname = \"Acme\"\npath = \"/nonexistent/acme\"\nmetrics = \"acme.json\"\n";
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(manifest_toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");
    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("low (1-5)"), "{md}");
    assert!(md.contains("high (>20)"), "{md}");
    assert!(md.contains("80%"), "80/100 buckets:\n{md}");
    assert!(md.contains("20%"), "20/100 buckets:\n{md}");
    assert!(
        md.contains("```mermaid"),
        "chart emitted for complexity dataset:\n{md}"
    );
}

/// Why: a bare scan-only run has NO complexity data — the dataset must stay
/// empty (omit-empty) rather than fabricating buckets from nothing, and no
/// chart should render for it, even while a SIBLING repo-derivable dataset
/// (`loc_by_technology`) in the same §7 section DOES populate and chart.
/// What: a repo with a real scanned checkout but no `metrics` key; asserts no
/// complexity bucket data leaks in and exactly one `xychart-beta` chart renders
/// (the loc_by_technology one) — proving complexity contributed no chart.
/// Test: this test itself.
#[test]
fn complexity_distribution_empty_without_metrics() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).expect("mkdir");
    git_repo_with_languages(&repo_dir);

    let manifest_toml = format!(
        "[report]\ntitle = \"No Complexity\"\n\n[[repositories]]\nname = \"Acme\"\npath = \"{}\"\n",
        repo_dir.display()
    );
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(&manifest_toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");
    let template = TemplateLoader::new()
        .load("report-technical-dd")
        .expect("template loads");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(!md.contains("low ("), "no fabricated bucket label:\n{md}");
    assert_eq!(
        md.matches("xychart-beta").count(),
        1,
        "only the sibling loc_by_technology chart renders:\n{md}"
    );
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

// ─── #5318: §2 renders without synthesis ─────────────────────────────────────

/// The rendered §2 body, from its heading up to the next `##` heading
/// (so it includes the `### Top Risks` child).
fn executive_summary_section(md: &str) -> String {
    let start = md
        .find("## 2. Executive Summary")
        .unwrap_or_else(|| panic!("no §2 heading in:\n{md}"));
    let rest = &md[start..];
    let end = rest[3..].find("\n## ").map(|i| i + 3).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Just the §2 paragraph — the heading through the `### Top Risks` child, which
/// has its own data source and its own emptiness verdict.
fn executive_summary_paragraph(md: &str) -> String {
    let section = executive_summary_section(md);
    match section.find("\n### ") {
        Some(i) => section[..i].to_string(),
        None => section,
    }
}

/// Why: issue #5318 — every `tga audit` report collapsed §2 to
/// `_No data available — see Gaps & Caveats._` because the executive summary
/// was filled ONLY from `--synthesize` prose, while the same report listed a
/// RED and an AMBER finding in §5. This is the regression guard: a
/// deterministic run over findings data must populate §2.
/// What: renders the findings fixture with NO synthesis attached and asserts
/// the paragraph, the severity counts, the Top Risks rows, and the absence of
/// the collapse line anywhere in §2.
/// Test: this test itself.
#[test]
fn reporter_fills_executive_summary_without_synthesis() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_findings(tmp.path());
    assert!(model.synthesis.is_none(), "fixture must be synthesis-free");

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    let section = executive_summary_section(&md);

    assert!(
        !section.contains("No data available"),
        "§2 collapsed on a run that had findings:\n{section}"
    );
    assert!(
        section.contains("Repository inspection covered 1 application (Acme Web)"),
        "no roll-up paragraph:\n{section}"
    );
    assert!(
        section.contains("1 RED (critical) and 1 AMBER (medium-risk) findings"),
        "severity counts missing:\n{section}"
    );
    // Top Risks fills from the same findings, RED first.
    assert!(
        section.contains("SQL injection"),
        "RED finding missing from Top Risks:\n{section}"
    );
    assert!(
        section.contains("Stale dependency"),
        "AMBER finding missing from Top Risks:\n{section}"
    );
    assert!(!md.contains("{{"), "no raw placeholder survives");
}

/// Why: closure condition 2 of #5318 — when §2 genuinely cannot be produced the
/// report must name the missing input rather than pointing at Gaps & Caveats.
/// What: renders a remote-only manifest (nothing to scan, no metrics) and
/// asserts the §2 paragraph names each absent input instead of collapsing. The
/// `### Top Risks` child legitimately stays empty here — a report with no
/// findings has no risks to rank — so the assertion is scoped to the paragraph.
/// Test: this test itself.
#[test]
fn reporter_names_missing_inputs_when_nothing_measured() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let toml = r#"
        [report]
        title = "Remote Only"

        [[repositories]]
        name = "Remote App"
        remote = "acme/remote-app"
    "#;
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    let paragraph = executive_summary_paragraph(&md);

    assert!(
        !paragraph.contains("No data available"),
        "§2 must state the missing input, not collapse:\n{paragraph}"
    );
    assert!(paragraph.contains("`metrics` file"), "got:\n{paragraph}");
    assert!(paragraph.contains("`--analyze`"), "got:\n{paragraph}");
}

/// Why: the deterministic roll-up is a floor, never a replacement — verified
/// synthesis prose must still win, and the Top Risks table must never carry
/// both sets of rows.
/// What: attaches an available synthesis with its own exec summary and one top
/// risk over a fixture that also has findings, then asserts the synthesized
/// prose renders, the roll-up does not, and exactly one risk row is present.
/// Test: this test itself.
#[test]
fn reporter_prefers_synthesis_over_deterministic_summary() {
    use crate::report::synthesize::{RiskRow, Synthesis};

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model_with_findings(tmp.path());
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: Some("An acquirer-relevant judgement.".to_string()),
        top_risks: vec![RiskRow {
            description: "Credential exposure".to_string(),
            severity: "RED".to_string(),
            cost: "2 weeks".to_string(),
            apps: "Acme Web".to_string(),
        }],
        findings: vec![],
        notes: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);
    let section = executive_summary_section(&md);

    assert!(section.contains("An acquirer-relevant judgement."));
    assert!(
        !section.contains("Repository inspection covered"),
        "synthesis must overwrite the roll-up:\n{section}"
    );
    assert!(section.contains("Credential exposure"));
    assert!(
        !section.contains("SQL injection"),
        "deterministic rows must not stack under synthesized rows:\n{section}"
    );
}

/// Why: the Top Risks table is capped at five rows, and an acquirer who skims
/// the table without the paragraph above it would otherwise read those five as
/// the entire risk picture. A silently capped top-risks table has shipped as a
/// defect once already (#2373).
/// What: renders seven RED/AMBER findings and asserts the rendered table carries
/// the "Top 5 of 7" caption row; then renders a fixture with two findings and
/// asserts no caption appears.
/// Test: this test itself.
#[test]
fn reporter_captions_a_truncated_top_risks_table() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let metrics = r#"{
      "findings": [
        { "title": "A1", "severity": "amber", "category": "x", "component": "a1.rs" },
        { "title": "A2", "severity": "amber", "category": "x", "component": "a2.rs" },
        { "title": "A3", "severity": "amber", "category": "x", "component": "a3.rs" },
        { "title": "A4", "severity": "amber", "category": "x", "component": "a4.rs" },
        { "title": "A5", "severity": "amber", "category": "x", "component": "a5.rs" },
        { "title": "R1", "severity": "red", "category": "security", "component": "r1.rs" },
        { "title": "R2", "severity": "red", "category": "security", "component": "r2.rs" }
      ]
    }"#;
    std::fs::write(tmp.path().join("acme.json"), metrics).expect("write metrics");
    let toml = r#"
        [report]
        title = "Acme Due Diligence"

        [[repositories]]
        name = "Acme Web"
        path = "/nonexistent/acme-web"
        metrics = "acme.json"
    "#;
    let manifest_path = tmp.path().join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    let model = ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("model builds");

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let section = executive_summary_section(&Reporter::new(tmp.path()).render(&model, &template));

    assert!(
        section.contains("**Top 5 of 7**"),
        "truncated table must caption itself:\n{section}"
    );
    assert!(
        section.contains("2 further RED/AMBER finding(s) are not listed here"),
        "caption must name what is missing:\n{section}"
    );
    assert!(
        !section.contains("| 6 |"),
        "the caption row must not be numbered as a sixth risk:\n{section}"
    );

    // A fixture the cap never touches must carry no caption.
    let untruncated = fixture_model_with_findings(tmp.path());
    let plain =
        executive_summary_section(&Reporter::new(tmp.path()).render(&untruncated, &template));
    assert!(
        !plain.contains("Top 5 of"),
        "an untruncated table must not claim truncation:\n{plain}"
    );
}

/// Build a model whose manifest declares a ticketing artifact (#5405).
///
/// `template` is the name recorded on the model; the caller loads the same
/// template's text separately, so both bundled templates run this fixture.
fn fixture_model_with_ticketing(dir: &Path, template: &str, artifact: &str) -> ReportModel {
    std::fs::write(dir.join("ticketing.json"), artifact).expect("write ticketing");

    let toml = r#"
        [report]
        title = "Acme Due Diligence"
        analyst = "bobmatnyc"
        ticketing = "ticketing.json"

        [[repositories]]
        name = "Acme Web"
        path = "/nonexistent/acme-web"
    "#;
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    ReportModel::build(&manifest, &manifest_path, template, None).expect("build model")
}

/// The figures-present case, asserted on the RENDERED artifact rather than on
/// the model: the report reads `work_items` (by way of the correlation figures
/// tga wrote) and states them on the page.
///
/// The end-to-end shape matters here. Every intermediate layer could hold the
/// figures correctly and the section could still never reach the page — the
/// polish pass drops marker rows and collapses emptied sections, so a scalar
/// checked only on the model would prove nothing about the document an acquirer
/// reads. Both bundled templates place the section at §8, so one body serves
/// both.
fn assert_template_states_ticketing_coverage(template_name: &str) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_ticketing(
        tmp.path(),
        template_name,
        r#"{"schema_version":"v0","commits":412,"commits_linked":260,
            "work_items":180,"work_items_linked":155,"sources":["jira","linear"]}"#,
    );
    let template = TemplateLoader::bundled_only()
        .load(template_name)
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(
        md.contains("## 8. Ticketing & Delivery Traceability"),
        "{template_name}: the section must render:\n{md}"
    );
    assert!(
        md.contains("260 of 412 commit(s)") && md.contains("jira, linear"),
        "{template_name}: the counts and the boards they came from must reach the page:\n{md}"
    );
    // Not collapsed, and not swept into the gaps list.
    assert!(
        !md.contains("## 8. Ticketing & Delivery Traceability\n\n_No data available"),
        "{template_name}: a populated section must not collapse:\n{md}"
    );
    assert!(!md.contains("{{ticketing_coverage}}"));
}

/// The anti-fail-open half, also on the rendered artifact: a run that
/// correlated nothing STATES that. A zero is a finding about the codebase —
/// commits do not cite tracked work — and must never render as the same blank
/// a missing artifact produces.
fn assert_template_states_a_zero_coverage_run(template_name: &str) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_ticketing(
        tmp.path(),
        template_name,
        r#"{"schema_version":"v0","commits":300,"commits_linked":0,
            "work_items":12,"work_items_linked":0,"sources":[]}"#,
    );
    let template = TemplateLoader::bundled_only()
        .load(template_name)
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(
        md.contains("No commit referenced a tracked board item"),
        "{template_name}: a zero run must state itself, not collapse:\n{md}"
    );
    assert!(md.contains("300 commit(s)"), "{template_name}:\n{md}");
}

/// The other side of that contract: when the producing run supplied NO
/// artifact, the section must be named as unassessed rather than silently
/// absent — DOC-67 §9's rule, and the defect #5405 is one level up from.
///
/// This is the case a template can fail invisibly. A template with no section
/// at all renders no placeholder, so the polish pass has nothing to collapse
/// and nothing to name: the omission escapes the very mechanism built to
/// surface it. Assert the heading, the collapse line, and the gap entry
/// together — any one of the three alone passes on a template that dropped the
/// dimension entirely.
fn assert_template_names_a_missing_ticketing_artifact(template_name: &str) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load(template_name)
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    // The heading survives, carrying the collapse note — the reader sees that
    // the dimension exists and was not assessed.
    assert!(
        md.contains("## 8. Ticketing & Delivery Traceability"),
        "{template_name}: the heading must survive so the omission is visible:\n{md}"
    );
    let section = md
        .split("## 8. Ticketing & Delivery Traceability")
        .nth(1)
        .expect("section body");
    assert!(
        section.contains("_No data available — see Gaps & Caveats._"),
        "{template_name}: an unpopulated section must say so:\n{section}"
    );
    // And it is listed among the gaps, not merely blank on the page.
    let gaps = md
        .split("## 9. Gaps & Caveats")
        .nth(1)
        .expect("gaps section");
    assert!(
        gaps.contains("Ticketing & Delivery Traceability"),
        "{template_name}: the unassessed dimension must be named in Gaps & Caveats:\n{gaps}"
    );
}

#[test]
fn reporter_states_ticketing_coverage() {
    assert_template_states_ticketing_coverage("report-technical-dd");
}

#[test]
fn reporter_states_a_zero_coverage_run_rather_than_omitting_it() {
    assert_template_states_a_zero_coverage_run("report-technical-dd");
}

#[test]
fn a_missing_ticketing_artifact_is_named_not_silently_dropped() {
    assert_template_names_a_missing_ticketing_artifact("report-technical-dd");
}

/// The CAST template's copy of the figures-present case.
///
/// A CAST engagement resolves the same manifest and the same model: nothing on
/// the load path is template-aware, so the figures reach `build_scope`
/// identically. Only the template decides whether they reach the page.
#[test]
fn cast_reporter_states_ticketing_coverage() {
    assert_template_states_ticketing_coverage("report-technical-dd-cast");
}

/// The CAST template's copy of the correlated-nothing case.
#[test]
fn cast_reporter_states_a_zero_coverage_run_rather_than_omitting_it() {
    assert_template_states_a_zero_coverage_run("report-technical-dd-cast");
}

/// The CAST template's copy of the missing-artifact case — the one that was
/// failing invisibly. The template had no §8 at all, so a CAST report showed
/// neither the coverage figures nor a gap line explaining their absence.
#[test]
fn a_missing_ticketing_artifact_is_named_in_the_cast_template_too() {
    assert_template_names_a_missing_ticketing_artifact("report-technical-dd-cast");
}

/// #6029 regression: a sweep run WITHOUT `--analyze` carries no
/// `AnalyzeMetrics`, but `RepoScan` is built on every model build — and the
/// rendered Key Facts block must show that LoC figure. Before the fix
/// `fill_key_facts` read `metrics` alone, so every row fell to the honesty
/// marker, the omit-empty pass dropped all of them, and the polish pass
/// collapsed the whole block to `_No data available — see Gaps & Caveats._`
/// while the scan held 1.5M LoC. Assert the heading, the figure, and the
/// absence of the collapse line together — the figure alone would pass on a
/// report that had also dropped the heading.
#[test]
fn key_facts_renders_scan_loc_without_analyze_metrics() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    // Reproduce the no-`--analyze` shape: scan data present, metrics absent.
    let repo = model.repositories.first_mut().expect("one repository");
    repo.metrics = None;
    repo.scan = Some(crate::report::scan::RepoScan {
        total_loc: 1_500_000,
        file_count: 8_432,
        by_language: vec![crate::report::metrics::LanguageLoc {
            language: "Rust".to_string(),
            loc: 1_500_000,
        }],
        frameworks: vec![],
    });
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("## Key Facts"), "{md}");
    let section = md.split("## Key Facts").nth(1).expect("key facts body");
    let block = section.split("## 2.").next().expect("block before §2");
    assert!(
        block.contains("1500000"),
        "the scan's LoC figure must reach Key Facts:\n{block}"
    );
    assert!(
        block.contains("8432"),
        "the scan's file count must reach Key Facts:\n{block}"
    );
    assert!(
        !block.contains("No data available"),
        "Key Facts must never collapse while the sweep holds data:\n{block}"
    );
    // A genuinely absent input names itself in its own row rather than
    // blanking the block around the data that IS present.
    assert!(
        block.contains("--analyze"),
        "the complexity row must name its missing input:\n{block}"
    );
    assert!(
        block.contains("authorship artifact"),
        "the author rows must name their missing input:\n{block}"
    );
}

/// #6029 regression: `fill_key_facts` writes named-gap text into
/// `facts_author_count`/`facts_trajectory` unconditionally, so the measured
/// figures reach the report only because `reporter::build_scope` runs
/// `fill_authorship_facts` AFTER it (reporter.rs:236, 239). Nothing else in the
/// suite exercises both together — `completes_key_facts_author_rows` calls
/// `fill_authorship_facts` alone, and no other full-render test loads an
/// authorship artifact — so swapping the two calls would clobber measured
/// authorship data with "Not computed" and leave every test green. This renders
/// the whole report with an artifact loaded and asserts the measured figures
/// win in the Key Facts table.
#[test]
fn key_facts_authorship_rows_survive_the_fill_order() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model(tmp.path());
    let repo = model.repositories.first_mut().expect("one repository");
    repo.authorship = Some(crate::report::authorship::AuthorshipSummary {
        schema_version: "v0".to_string(),
        repository: "Acme Web".to_string(),
        distinct_authors: 7,
        bus_factor: 2,
        top_author_share_pct: 61.0,
        single_author_subsystems: vec!["src".to_string()],
        monthly_trajectory: vec![
            crate::report::authorship::MonthlyActivity {
                month: "2026-01".to_string(),
                active_authors: 1,
                commits: 5,
            },
            crate::report::authorship::MonthlyActivity {
                month: "2026-02".to_string(),
                active_authors: 2,
                commits: 40,
            },
        ],
        unresolved_authors: 0,
        caveats: vec![],
    });

    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    let section = md.split("## Key Facts").nth(1).expect("key facts body");
    let block = section.split("## 2.").next().expect("block before §2");

    let authors = block
        .lines()
        .find(|l| l.starts_with("| Number of authors |"))
        .expect("author-count row");
    assert!(
        authors.contains('7'),
        "the measured author count must win over the named gap:\n{authors}"
    );
    assert!(
        !authors.contains("Not computed"),
        "the named gap clobbered the measured author count:\n{authors}"
    );

    let trajectory = block
        .lines()
        .find(|l| l.starts_with("| 12-month trajectory |"))
        .expect("trajectory row");
    assert!(
        trajectory.contains("increasing"),
        "the measured trajectory must win over the named gap:\n{trajectory}"
    );
    assert!(
        !trajectory.contains("Not computed"),
        "the named gap clobbered the measured trajectory:\n{trajectory}"
    );
}

// ── #6046: the code-review / authorship output split ─────────────────────────

/// Build a model whose single repo carries a loaded authorship artifact, so the
/// Authorship & Key-Person Risk section has real rows and survives `polish`.
fn fixture_model_with_authorship(dir: &Path) -> ReportModel {
    let mut model = fixture_model(dir);
    let repo = model.repositories.first_mut().expect("one repository");
    repo.authorship = Some(crate::report::authorship::AuthorshipSummary {
        schema_version: "v0".to_string(),
        repository: "Acme Web".to_string(),
        distinct_authors: 7,
        bus_factor: 2,
        top_author_share_pct: 61.0,
        single_author_subsystems: vec!["src".to_string()],
        monthly_trajectory: vec![crate::report::authorship::MonthlyActivity {
            month: "2026-02".to_string(),
            active_authors: 2,
            commits: 40,
        }],
        unresolved_authors: 0,
        caveats: vec!["squash-merge attribution is not corrected for".to_string()],
    });
    model
}

/// Why: the owner asked to read the code review apart from authorship (#6046),
/// so one render must produce two documents with disjoint section membership.
/// What: renders a model with authorship data and asserts the section is absent
/// from the code-review document — including its jump list — and present, with
/// its measured rows, in the authorship document.
/// Test: this test itself.
#[test]
fn render_splits_authorship_into_its_own_document() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model_with_authorship(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");

    let documents = Reporter::new(tmp.path()).render_documents(&model, &template);

    assert!(
        !documents
            .code_review
            .contains("## Authorship & Key-Person Risk"),
        "the authorship section must not render in the code-review document:\n{}",
        documents.code_review
    );
    assert!(
        !documents.code_review.contains("| Acme Web | 7"),
        "the authorship row must not render in the code-review document:\n{}",
        documents.code_review
    );
    assert!(
        !documents
            .code_review
            .contains("#authorship-key-person-risk"),
        "the jump list must not link a section this document no longer carries"
    );
    // The frontloaded sections stay where the owner put them (#6004).
    assert!(documents.code_review.contains("## Key Facts"));
    assert!(documents.code_review.contains("## 2. Executive Summary"));
    assert!(documents.code_review.contains("## 5. Findings by Severity"));

    let authorship = documents.authorship.expect("authorship document rendered");
    assert!(
        authorship.starts_with("# Authorship & Key-Person Risk: Acme Due Diligence"),
        "{authorship}"
    );
    assert!(authorship.contains("| Acme Web |"), "{authorship}");
    assert!(authorship.contains("squash-merge"), "{authorship}");
    assert!(
        !authorship.contains("Findings by Severity"),
        "the authorship document must carry only its own section:\n{authorship}"
    );
}

/// Why: a run with no authorship data must still deliver the document — an
/// absent file reads as "we forgot", where `polish`'s own no-data line reads as
/// "we looked and there was nothing", which is what the Gaps & Caveats entry in
/// the code-review report also says.
/// What: the no-authorship fixture still renders a second document, carrying the
/// collapsed section's no-data line rather than fabricated rows.
/// Test: this test itself.
#[test]
fn render_without_authorship_data_still_produces_the_document() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = fixture_model(tmp.path());
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");

    let documents = Reporter::new(tmp.path()).render_documents(&model, &template);
    let authorship = documents.authorship.expect("authorship document rendered");
    assert!(authorship.contains("No data available"), "{authorship}");
    assert!(
        !documents
            .code_review
            .contains("## Authorship & Key-Person Risk"),
        "the section must leave the code-review document even with no data"
    );
}

/// Why: the split is only delivered once both documents reach disk — tga reads
/// the paths trusty-review prints, and trusty-audit packages every file in the
/// output directory.
/// What: writes a model with authorship data and asserts a third file appears,
/// named `{stem}-authorship.md`, with the code-review report still first.
/// Test: this test itself.
#[test]
fn write_emits_the_authorship_document_alongside() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model_with_authorship(tmp.path());
    model.synthesis = Some(crate::report::synthesize::Synthesis::default());
    let out_dir = tmp.path().join("out");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");

    let written = Reporter::new(&out_dir)
        .write(&model, &template)
        .expect("write ok");

    assert_eq!(written.len(), 3, "{written:?}");
    let stem = report_stem(&model);
    assert_eq!(written[0], out_dir.join(format!("{stem}.md")));
    assert_eq!(written[1], out_dir.join(format!("{stem}.json")));
    assert_eq!(written[2], out_dir.join(format!("{stem}-authorship.md")));
    for path in &written {
        assert!(path.exists(), "{} was not written", path.display());
    }

    let authorship = std::fs::read_to_string(&written[2]).expect("read authorship");
    assert!(
        authorship.contains("Authorship & Key-Person Risk"),
        "{authorship}"
    );
    let code_review = std::fs::read_to_string(&written[0]).expect("read code review");
    assert!(
        !code_review.contains("## Authorship & Key-Person Risk"),
        "{code_review}"
    );
}

// ─── #6082 lap 5: title-collision merge ───────────────────────────────────────

/// Three AMBER hotspots shaped like the graded report's: TWO share the canned
/// analyze title "Split oversized impl block" against different files, which is
/// what made title an ambiguous merge key.
fn fixture_model_with_colliding_titles(dir: &Path) -> ReportModel {
    let metrics = r#"{
      "loc": { "total": 5000, "by_language": [ { "language": "Rust", "loc": 5000 } ] },
      "counts": { "files": 20, "functions": 150 },
      "findings": [
        { "title": "Split oversized impl block", "severity": "amber", "category": "maintainability",
          "component": "crates/trusty-common/src/memory_core/store/hnsw_store.rs",
          "description": "cyclomatic complexity 140 (grade F); long_impl_block" },
        { "title": "Extract method — dispatch", "severity": "amber", "category": "maintainability",
          "component": "crates/trusty-common/src/tickets/server.rs",
          "description": "cyclomatic complexity 118 (grade F); long_function" },
        { "title": "Split oversized impl block", "severity": "amber", "category": "maintainability",
          "component": "crates/trusty-common/src/tickets/api/backends/jira/backend.rs",
          "description": "cyclomatic complexity 112 (grade F); long_impl_block" }
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

/// One AMBER narrative, with the fields each test varies left to the caller.
fn amber_prose(
    title: &str,
    component: &str,
    evidence: &str,
) -> crate::report::synthesize::FindingProse {
    crate::report::synthesize::FindingProse {
        trace_verdict: String::new(),
        app_slug: String::new(),
        title: title.to_string(),
        severity: "AMBER".to_string(),
        description: "the block has grown past what one reader can hold".to_string(),
        evidence: evidence.to_string(),
        component: component.to_string(),
        business_impact: String::new(),
        remediation: "decompose it".to_string(),
        cost_effort: "moderate".to_string(),
        evidence_measured: false,
    }
}

/// Render the colliding-title fixture with `findings` attached as synthesis.
fn render_colliding(findings: Vec<crate::report::synthesize::FindingProse>) -> String {
    use crate::report::synthesize::Synthesis;

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut model = fixture_model_with_colliding_titles(tmp.path());
    let slug = model.repositories[0].slug.clone();
    let findings = findings
        .into_iter()
        .map(|mut f| {
            f.app_slug = slug.clone();
            f
        })
        .collect();
    model.synthesis = Some(Synthesis {
        code_quality_summary: None,
        security_summary: None,
        authorship_summary: None,
        executive_summary: None,
        top_risks: vec![],
        findings,
        notes: vec![],
    });
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    Reporter::new(tmp.path()).render(&model, &template)
}

/// Section 5.2, the AMBER findings list. The hotspot tables elsewhere in the
/// report repeat a finding's metrics fields, so a document-wide search cannot
/// tell a rendered finding from a dropped one.
fn amber_section(md: &str) -> &str {
    md.split("### 5.2")
        .nth(1)
        .and_then(|s| s.split("### 5.3").next())
        .expect("AMBER section present")
}

/// The Component recorded in the same finding block as `needle` — the last
/// `**Component:**` line before it, which is the one belonging to that block.
fn component_for(md: &str, needle: &str) -> String {
    let at = md
        .find(needle)
        .unwrap_or_else(|| panic!("'{needle}' never rendered:\n{md}"));
    let before = &md[..at];
    let marker = "**Component:**";
    let start = before
        .rfind(marker)
        .unwrap_or_else(|| panic!("no component line precedes '{needle}'"))
        + marker.len();
    before[start..]
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// #6082 lap 5 (BLOCKER 1): two narratives sharing one title must land on their
/// own components.
///
/// Why: matching on title text alone sent both narratives to the FIRST row
/// carrying that title. The graded report's AMBER #1 rendered `hnsw_store.rs`
/// as its Component under the Jira backend's narrative, and the hnsw narrative
/// was never rendered at all — a silent cross-wire, exit 0.
/// What: two AMBER metrics rows share the canned title against different files;
/// each narrative names its own file. Asserts each business impact renders
/// against the Component it belongs to.
/// Test: this test itself.
#[test]
fn colliding_titles_attach_to_their_own_components() {
    let mut hnsw = amber_prose(
        "Split oversized impl block",
        "crates/trusty-common/src/memory_core/store/hnsw_store.rs:327",
        "fn insert(&mut self, id: u64) {",
    );
    hnsw.business_impact = "vector maintenance carries the deepest risk".to_string();
    let mut jira = amber_prose(
        "Split oversized impl block",
        "crates/trusty-common/src/tickets/api/backends/jira/backend.rs:368",
        "let jql = format!(\"project = {}\", key);",
    );
    jira.business_impact = "ticket sync carries the second risk".to_string();

    let md = render_colliding(vec![hnsw, jira]);
    let amber = amber_section(&md);

    assert!(
        component_for(amber, "vector maintenance carries the deepest risk")
            .contains("hnsw_store.rs"),
        "the hnsw narrative must sit on the hnsw row:\n{amber}"
    );
    assert!(
        component_for(amber, "ticket sync carries the second risk").contains("jira/backend.rs"),
        "the jira narrative must sit on the jira row:\n{amber}"
    );
}

/// #6082 lap 5 (BLOCKER 1, second arm): a self-restating narrative must never
/// delete the measurement underneath it.
///
/// Why: this is a fail-open branch — the graded render dropped
/// `tickets/server.rs` (cyclomatic 118, grade F) entirely and still exited 0,
/// listing 167 of the 168 AMBER findings the metrics measured. The narrative
/// was junk; the hotspot was not.
/// What: a narrative whose evidence quotes its own file path, against a
/// metrics-backed row. Asserts the metrics title and description still render,
/// and that the refused narrative's own prose does not.
/// Test: this test itself.
#[test]
fn a_self_restating_narrative_never_deletes_its_metrics_row() {
    let mut restating = amber_prose(
        "Extract method — dispatch",
        "trusty-common tickets server",
        "crates/trusty-common/src/tickets/server.rs (extract method — dispatch)",
    );
    restating.business_impact = "harder to change over time".to_string();

    let md = render_colliding(vec![restating]);
    // Scoped to 5.2: the metrics description also appears in the hotspot
    // tables, so a document-wide search would pass even with the row dropped.
    let amber = amber_section(&md);

    assert!(
        amber.contains("Extract method — dispatch"),
        "the finding must still be listed:\n{amber}"
    );
    assert!(
        amber.contains("cyclomatic complexity 118 (grade F)"),
        "the measured hotspot must survive a junk narrative:\n{amber}"
    );
    assert!(
        !amber.contains("harder to change over time"),
        "the restating narrative must still be refused:\n{amber}"
    );
}

/// #6082 lap 6: an unmatched narrative citing a topic phrase must not be
/// numbered as a finding.
///
/// Why: the graded report listed 156 AMBER items against the 155 AMBER findings
/// its own metrics measured. The extra one — "Split oversized impl block
/// (hnsw_store)", component `trusty-common (memory_core/store)`, Evidence body
/// the file's own path — is the exact shape `is_self_restatement` exists to
/// refuse, and it slipped through because the parenthesised half of that topic
/// phrase carries a slash, so `names_a_file` read it as a path.
/// What: the live orphan's shape against the three-row colliding-title fixture.
/// Asserts the numbered list stops at the third measured row, that the orphan's
/// prose does not render, and that Synthesis Status names it.
/// Test: this test itself.
#[test]
fn an_unplaced_narrative_is_disclosed_not_numbered() {
    let mut orphan = amber_prose(
        "Split oversized impl block (hnsw_store)",
        "trusty-common (memory_core/store)",
        "crates/trusty-common/src/memory_core/store/hnsw_store.rs",
    );
    orphan.business_impact = "vector-store logic sits in one oversized block".to_string();

    let md = render_colliding(vec![orphan]);
    let amber = amber_section(&md);

    assert!(
        amber.contains("\n3. **"),
        "the three measured rows must still render:\n{amber}"
    );
    assert!(
        !amber.contains("\n4. **"),
        "the unplaced narrative must not be numbered:\n{amber}"
    );
    assert!(
        !amber.contains("vector-store logic sits in one oversized block"),
        "the orphan's prose must not render as a finding:\n{amber}"
    );

    let status = md
        .split("## Synthesis Status")
        .nth(1)
        .expect("Synthesis Status section present");
    assert!(
        status.contains("'Split oversized impl block (hnsw_store)'"),
        "the dropped narrative must be disclosed:\n{status}"
    );
    // #6082 lap 7: the disclosure names the finding, not the matcher. The
    // hnsw_store row is the first AMBER row this fixture renders, which is the
    // position it holds in the graded report too.
    assert!(
        status.contains("section 5.2, AMBER finding 1:"),
        "the disclosure must cross-reference the finding number:\n{status}"
    );
    assert!(
        status.contains(
            "was withheld because it could not be verified against the collected data, so \
             that finding shows the measured data only"
        ),
        "the disclosure must state why in reader-facing terms:\n{status}"
    );
    assert!(
        !status.contains("matches no measured finding"),
        "the pipeline-debug wording must be gone:\n{status}"
    );
}

/// #6082 lap 7: the closing signature is the last content in a full render.
///
/// Why: the graded report signed off at line 1883 and carried Synthesis Status,
/// Dependency Inventory and Investigation Coverage after it. A reader who stops
/// at a signature — which is what a signature is for — missed the coverage
/// record that qualifies every finding above it.
/// Test: this test itself.
#[test]
fn the_signature_is_the_last_content_in_the_document() {
    let md = render_colliding(vec![amber_prose(
        "Split oversized impl block",
        "crates/trusty-common/src/memory_core/store/hnsw_store.rs:41",
        "let mut guard = self.index.write();",
    )]);
    let last = md
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .expect("a last line");
    assert!(
        last.starts_with("*Source:"),
        "the signature block must close the document, found: {last}\n---\n{md}"
    );
    assert!(
        md.find("## Synthesis Status").expect("status section")
            < md.find("*Generated by trusty-review report analysis")
                .expect("signature"),
        "every appended section must precede the signature"
    );
}

/// #6082 lap 6, other arm: a narrative that cites a real file and quotes a line
/// out of it still renders on its own.
///
/// Why: most of the graded report's findings arrive this way — the investigation
/// injects a metrics row per verified finding, but a narrative whose title the
/// row does not carry still has a file, a line and a quote behind it. Refusing
/// every unmatched narrative would delete those, which is the failure the lap-5
/// fix was about.
/// Test: this test itself.
#[test]
fn a_narrative_citing_a_real_file_still_renders() {
    let mut verified = amber_prose(
        "compact_orphans deletes across three transactions",
        "crates/trusty-common/src/memory_core/store/hnsw_store.rs:221",
        "let live = self.live_ids()?;",
    );
    verified.business_impact = "a just-inserted vector can be silently deleted".to_string();

    let amber_md = render_colliding(vec![verified]);
    let amber = amber_section(&amber_md);

    assert!(
        amber.contains("a just-inserted vector can be silently deleted"),
        "a verified narrative with no metrics row must still render:\n{amber}"
    );
    assert!(
        amber.contains("\n4. **"),
        "it is numbered alongside the three measured rows:\n{amber}"
    );
}
