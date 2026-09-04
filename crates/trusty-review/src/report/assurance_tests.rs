//! Tests for the Assurance Scans section (#6075).
//!
//! Why: the section exists to stop a false clean claim, so both directions need
//! pinning — a declared finding must reach the rendered report, and a manifest
//! declaring none must leave the report byte-identical to one produced before
//! the section existed.
//! What: the end-to-end render through `Reporter::render`, the empty case, the
//! band ordering, the per-collector grouping #6076/#6077 reuse, and the cell
//! escaping an upstream advisory title can break.
//! Test: this file.

use std::path::Path;

use super::report_section;
use crate::report::Reporter;
use crate::report::manifest::parse_manifest;
use crate::report::model::ReportModel;
use crate::report::template::TemplateLoader;

/// A manifest declaring `count` assurance findings, plus one repository.
fn manifest_declaring(findings: &str) -> String {
    format!(
        "[report]\ntitle = \"Acme Due Diligence\"\n{findings}\n\n\
         [[repositories]]\nname = \"Acme Web\"\npath = \"/nonexistent/acme-web\"\n"
    )
}

fn model_from(dir: &Path, toml: &str) -> ReportModel {
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(toml, &manifest_path).expect("manifest parse");
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None).expect("model")
}

/// One `cargo audit` row, as `trusty_audit::grounding::cve::write_into` spells it.
const CVE_ROW: &str = "findings = [\n  { category = \"dependencies\", id = \"RUSTSEC-2024-0421\", \
     package = \"idna\", version = \"0.5.0\", severity = \"RED\", \
     title = \"Punycode labels that decode to pure ASCII\", \
     url = \"https://rustsec.org/advisories/RUSTSEC-2024-0421.html\" },\n]";

/// The deliverable: a finding trusty-audit wrote into the manifest reaches the
/// report an acquirer reads. Fails before #6075 — the key parsed nowhere and
/// nothing rendered it.
#[test]
fn a_declared_finding_reaches_the_report() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(tmp.path(), &manifest_declaring(CVE_ROW));
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("## Assurance Scans"), "{md}");
    assert!(md.contains("### Dependency CVE Exposure"), "{md}");
    assert!(md.contains("RUSTSEC-2024-0421"), "{md}");
    assert!(md.contains("idna"), "{md}");
    assert!(md.contains("0.5.0"), "{md}");
    assert!(md.contains("RED"), "{md}");
    assert!(
        md.contains("Punycode labels that decode to pure ASCII"),
        "{md}"
    );
    assert!(
        md.contains("https://rustsec.org/advisories/RUSTSEC-2024-0421.html"),
        "the advisory is linked so a reader can verify it: {md}"
    );
}

/// One `cargo-deny list` row, as `trusty_audit::grounding::license::write_into`
/// spells it (#6076).
const LICENSE_ROW: &str = "findings = [\n  { category = \"license\", id = \"AGPL-3.0-or-later\", \
     package = \"libfoo-sys\", version = \"0.9.1\", severity = \"RED\", \
     title = \"copyleft: linking this into a distributed work obliges releasing that work's \
     source under the same license\", url = \"https://spdx.org/licenses/AGPL-3.0-or-later.html\" \
     },\n]";

/// #6076's deliverable, through the same channel #6075 opened: a license
/// obligation trusty-audit wrote into the manifest reaches the report an
/// acquirer reads, under its own subsection heading.
#[test]
fn a_declared_license_finding_reaches_the_report() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(tmp.path(), &manifest_declaring(LICENSE_ROW));
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");
    let md = Reporter::new(tmp.path()).render(&model, &template);

    assert!(md.contains("## Assurance Scans"), "{md}");
    assert!(md.contains("### License / IP Exposure"), "{md}");
    assert!(md.contains("AGPL-3.0-or-later"), "{md}");
    assert!(md.contains("libfoo-sys"), "{md}");
    assert!(md.contains("RED"), "{md}");
    assert!(
        md.contains("obliges releasing that work's source under the same license"),
        "the obligation, not just the license name, reaches the page: {md}"
    );
    assert!(
        md.contains("https://spdx.org/licenses/AGPL-3.0-or-later.html"),
        "the license is linked so a reader can verify it: {md}"
    );
}

/// The narrowed disclaimer must point at the section that now carries the
/// answer, and must still deny everything the run genuinely does not cover.
#[test]
fn the_security_disclaimer_is_narrowed_rather_than_dropped() {
    let instruction = crate::report::section_instructions::default_instruction(
        crate::report::section_instructions::SECURITY_SUMMARY,
    )
    .expect("the security summary carries an instruction");

    assert!(
        instruction.contains("Assurance Scans"),
        "it points at the section that now reports CVE exposure: {instruction}"
    );
    for still_uncovered in ["SAST", "license review", "secrets scan", "penetration test"] {
        assert!(
            instruction.contains(still_uncovered),
            "`{still_uncovered}` is still not covered and must still be disclaimed: {instruction}"
        );
    }
    assert!(
        !instruction.contains("SAST/CVE/secrets/pen-test"),
        "the blanket denial is gone: {instruction}"
    );
}

/// A report whose manifest declares nothing is byte-identical to one produced
/// before this section existed — a heading over an empty table would read as a
/// scan that found nothing.
#[test]
fn no_findings_render_nothing_at_all() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(tmp.path(), &manifest_declaring(""));
    assert_eq!(report_section(&model), "");
}

/// RED before AMBER, whatever order the collector reported them in.
#[test]
fn the_worst_band_leads_the_table() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(
        tmp.path(),
        &manifest_declaring(
            "findings = [\n  { category = \"dependencies\", id = \"W-1\", package = \"atk\", \
             version = \"0.18.2\", severity = \"AMBER\", title = \"unmaintained\" },\n  \
             { category = \"dependencies\", id = \"V-1\", package = \"idna\", version = \"0.5.0\", \
             severity = \"RED\", title = \"exploitable\" },\n]",
        ),
    );
    let section = report_section(&model);
    let red = section.find("V-1").expect("the RED row renders");
    let amber = section.find("W-1").expect("the AMBER row renders");
    assert!(red < amber, "{section}");
}

/// The channel #6076 and #6077 reuse: a second category becomes its own
/// subsection rather than being folded into the CVE table.
#[test]
fn a_second_category_gets_its_own_subsection() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(
        tmp.path(),
        &manifest_declaring(
            "findings = [\n  { category = \"dependencies\", id = \"V-1\", package = \"idna\", \
             version = \"0.5.0\", severity = \"RED\", title = \"exploitable\" },\n  \
             { category = \"secrets\", id = \"S-1\", package = \"src/auth.rs\", version = \"—\", \
             severity = \"RED\", title = \"AWS key literal\" },\n]",
        ),
    );
    let section = report_section(&model);
    assert!(section.contains("### Dependency CVE Exposure"), "{section}");
    assert!(section.contains("### Secret Leakage"), "{section}");
    assert!(section.contains("S-1"), "{section}");
}

/// #6079's category has a heading a due-diligence reader recognises. Without the
/// mapping the section renders under the bare string `churn`, which reads as a
/// producer bug rather than as a measurement.
#[test]
fn the_churn_category_renders_under_its_own_heading() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(
        tmp.path(),
        &manifest_declaring(
            "findings = [\n  { category = \"churn\", id = \"churn-hotspot\", \
             package = \"src/api.rs\", version = \"\", severity = \"RED\", \
             title = \"31 commits by 4 author(s) in the last 180 days, +910/-402 lines\" },\n]",
        ),
    );
    let section = report_section(&model);
    assert!(section.contains("### Change Hotspots"), "{section}");
    assert!(!section.contains("### churn"), "{section}");
    assert!(section.contains("src/api.rs"), "{section}");
}

/// An advisory title is upstream prose. An unescaped pipe silently shifts every
/// later cell into the wrong column, which is worse than a missing row.
#[test]
fn a_pipe_in_an_advisory_title_does_not_break_the_table() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(
        tmp.path(),
        &manifest_declaring(
            "findings = [{ category = \"dependencies\", id = \"V-1\", package = \"p\", \
             version = \"1.0\", severity = \"RED\", title = \"a | b\" }]",
        ),
    );
    let section = report_section(&model);
    let row = section
        .lines()
        .find(|line| line.contains("V-1"))
        .expect("the row renders");
    assert!(row.contains("a \\| b"), "{row}");
    assert_eq!(
        row.match_indices(" | ").count(),
        4,
        "five columns, four separators: {row}"
    );
}

/// A row with no URL renders its id as plain text rather than an empty link.
#[test]
fn a_finding_with_no_url_renders_a_bare_id() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let model = model_from(
        tmp.path(),
        &manifest_declaring(
            "findings = [{ category = \"dependencies\", id = \"YANKED\", package = \"ghost\", \
             version = \"1.0.1\", severity = \"AMBER\", title = \"withdrawn from the registry\" }]",
        ),
    );
    let section = report_section(&model);
    assert!(section.contains("| YANKED |"), "{section}");
    assert!(!section.contains("]()"), "no empty link: {section}");
}
