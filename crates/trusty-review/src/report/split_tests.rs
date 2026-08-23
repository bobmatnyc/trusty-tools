//! Unit tests for the code-review / authorship output split (#6046).

use super::*;
use crate::report::Reporter;
use crate::report::manifest::parse_manifest;
use crate::report::model::ReportModel;
use crate::report::template::TemplateLoader;

/// A three-section document with the authorship section in the middle.
fn document() -> String {
    "# Report\n\n## 5. Findings\n\nA finding.\n\n## Authorship & Key-Person Risk\n\n\
     Bus factor 2.\n\n| App | Authors |\n|---|---|\n| acme | 7 |\n\n## 6. Risk Registers\n\nRows.\n"
        .to_string()
}

/// Why: the split is the whole feature — the authorship section must leave the
/// code-review document and arrive intact in the second one.
/// What: asserts the section is absent from the remainder, present in the cut,
/// and that the sections either side of it are untouched.
#[test]
fn splits_the_authorship_section() {
    let (kept, section) = split_authorship(&document());
    let section = section.expect("authorship section is present");

    assert!(!kept.contains("Authorship & Key-Person Risk"), "{kept}");
    assert!(!kept.contains("Bus factor 2"), "{kept}");
    assert!(kept.contains("## 5. Findings"), "{kept}");
    assert!(kept.contains("## 6. Risk Registers"), "{kept}");

    assert!(section.starts_with("## Authorship & Key-Person Risk"));
    assert!(section.contains("Bus factor 2"), "{section}");
    assert!(section.contains("| acme | 7 |"), "{section}");
    assert!(!section.contains("Risk Registers"), "{section}");
}

/// Why: a custom template without the section, or a run whose section `polish`
/// collapsed for want of data, must render exactly as before.
/// What: a document with no authorship heading comes back byte-identical, with
/// no second document.
#[test]
fn no_authorship_heading_leaves_the_document_whole() {
    let doc = "# Report\n\n## 5. Findings\n\nA finding.\n";
    let (kept, section) = split_authorship(doc);
    assert_eq!(kept, doc);
    assert!(section.is_none());
}

/// Why: raw-evidence quotes carry source bytes verbatim, and a shell comment or
/// markdown sample inside one can start with `## `. Cutting there would take the
/// rest of the document with it.
/// What: an authorship-looking heading inside a fenced block is not split on.
#[test]
fn a_fenced_heading_line_is_not_split_on() {
    let doc = "# Report\n\n## 5. Findings\n\n```\n## Authorship & Key-Person Risk\n```\n\nAfter.\n";
    let (kept, section) = split_authorship(doc);
    assert!(section.is_none(), "fenced line must not start a section");
    assert_eq!(kept, doc);
}

/// Why: the section may be the document's last, with nothing after it to bound
/// the cut.
/// What: a trailing authorship section splits to end-of-document.
#[test]
fn a_trailing_authorship_section_splits_to_the_end() {
    let doc = "# Report\n\n## 5. Findings\n\nA finding.\n\n## Authorship\n\nBus factor 2.\n";
    let (kept, section) = split_authorship(doc);
    assert_eq!(kept, "# Report\n\n## 5. Findings\n\nA finding.\n\n");
    assert!(
        section.expect("section").contains("Bus factor 2"),
        "trailing section must be captured"
    );
}

/// Why: the extracted section arrives with no title, date, or key to the
/// provenance markers its own cells carry — all of which lived in the
/// code-review document's header.
/// What: asserts the title line, the provenance legend, and the generation date.
#[test]
fn authorship_document_carries_title_and_legend() {
    let (_, section) = split_authorship(&document());
    let doc = authorship_document("Acme DD", "2026-08-19", &section.expect("section"), &[]);

    assert!(
        doc.starts_with("# Authorship & Key-Person Risk: Acme DD\n"),
        "{doc}"
    );
    assert!(doc.contains(provenance::LEGEND), "{doc}");
    assert!(doc.contains("generated 2026-08-19"), "{doc}");
}

/// Why: the wrapper must add a header and change nothing else — the rows are
/// already filled, polished, and provenance-tagged.
/// What: the section body survives verbatim and the duplicated `## ` heading is
/// promoted to the `# ` title rather than repeated.
#[test]
fn authorship_document_keeps_the_section_body() {
    let (_, section) = split_authorship(&document());
    let doc = authorship_document("Acme DD", "2026-08-19", &section.expect("section"), &[]);

    assert!(doc.contains("Bus factor 2."), "{doc}");
    assert!(doc.contains("| acme | 7 |"), "{doc}");
    assert!(
        !doc.contains("## Authorship & Key-Person Risk"),
        "the heading is promoted to the title, not repeated:\n{doc}"
    );
    assert!(doc.ends_with('\n'), "must end with a newline");
}

/// Why: `polish` collapses a data-less section to `_No data available — see Gaps
/// & Caveats._`, and since the split that referenced section renders in the
/// code-review document. A reader holding only the authorship document must
/// still learn why it is empty.
/// What: an authorship-load gap reaches the document as a bullet above the body.
#[test]
fn authorship_document_states_its_own_data_gaps() {
    let (_, section) = split_authorship(&document());
    let gaps = vec![
        "Authorship (Acme Web): could not load the authorship artifact (bad json). The report \
         states no authorship/key-person signal for this application."
            .to_string(),
        "Scan (Acme Web): the working tree was not readable.".to_string(),
    ];
    let doc = authorship_document("Acme DD", "2026-08-19", &section.expect("section"), &gaps);

    assert!(doc.contains("**Data gaps in this assessment:**"), "{doc}");
    assert!(
        doc.contains("could not load the authorship artifact"),
        "{doc}"
    );
    assert!(
        !doc.contains("the working tree was not readable"),
        "a non-authorship gap must not be misattributed to this document:\n{doc}"
    );
    assert!(
        doc.find("Data gaps").expect("gap block") < doc.find("Bus factor 2.").expect("body"),
        "the gaps must precede the body they explain:\n{doc}"
    );
}

/// Why: the gap block is failure-only — a successful run must render exactly as
/// it did before this addition.
/// What: a gap list carrying no authorship line produces no block at all.
#[test]
fn authorship_document_omits_the_gap_block_when_the_leg_succeeded() {
    let (_, section) = split_authorship(&document());
    let section = section.expect("section");
    let gaps = vec!["Scan (Acme Web): the working tree was not readable.".to_string()];

    let with_unrelated = authorship_document("Acme DD", "2026-08-19", &section, &gaps);
    let with_none = authorship_document("Acme DD", "2026-08-19", &section, &[]);

    assert_eq!(with_unrelated, with_none);
    assert!(!with_none.contains("Data gaps"), "{with_none}");
}

/// A one-repository manifest declaring `authorship = <name>`, built through the
/// real loader so the fail-open arm in `model.rs` is the thing under test.
fn model_declaring_authorship(dir: &std::path::Path, declared: &str) -> ReportModel {
    let toml = format!(
        r#"
        [report]
        title = "Acme Due Diligence"
        analyst = "bobmatnyc"

        [[repositories]]
        name = "Acme Web"
        path = "/nonexistent/acme-web"
        authorship = "{declared}"
    "#
    );
    let manifest_path = dir.join("manifest.toml");
    let manifest = parse_manifest(&toml, &manifest_path).expect("manifest parse");
    ReportModel::build(&manifest, &manifest_path, "report-technical-dd", None)
        .expect("a declared authorship path must never fail the build")
}

/// Why: the owner requirement behind #6046 — "I don't want the code review to be
/// held up by authorship." A broken authorship artifact is the cheapest real
/// failure to inject, and it must cost the code-review document nothing.
/// What: writes a malformed artifact, builds the model through
/// `ReportModel::build`, renders both documents, and asserts every code-review
/// section still arrives, with the failure named in both documents.
#[test]
fn an_authorship_load_failure_leaves_the_code_review_document_complete() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("authorship-0.json"), "{not json").expect("write artifact");
    let model = model_declaring_authorship(dir.path(), "authorship-0.json");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");

    let documents = Reporter::new(dir.path()).render_documents(&model, &template);

    let code = &documents.code_review;
    for heading in [
        "## 1. Report Metadata",
        "## 2. Executive Summary",
        "## 3. Scoring Model Normalization",
        "## 4. Per-Application Scorecard",
        "## 5. Findings by Severity",
        "## 6. Risk Registers",
        "## 7. Graph-Ready Data Appendix",
        "## 9. Gaps & Caveats",
    ] {
        assert!(code.contains(heading), "missing {heading}:\n{code}");
    }
    assert!(
        !code.contains("## Authorship & Key-Person Risk"),
        "the section still belongs to the other document:\n{code}"
    );
    assert!(
        code.contains("could not load the authorship artifact"),
        "the code-review report states the gap it hit:\n{code}"
    );

    let authorship = documents
        .authorship
        .expect("a failed leg still produces the document");
    assert!(
        authorship.contains("could not load the authorship artifact"),
        "the failure must be legible without the companion report:\n{authorship}"
    );
}

/// Why: the same guarantee for the OTHER authorship-leg failure shape — a
/// declared path that is not there at all.
/// What: the code-review document renders its executive summary and gap line,
/// and the render does not fail.
#[test]
fn a_missing_authorship_artifact_leaves_the_code_review_document_complete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = model_declaring_authorship(dir.path(), "not-written.json");
    let template = TemplateLoader::bundled_only()
        .load("report-technical-dd")
        .expect("bundled template");

    let documents = Reporter::new(dir.path()).render_documents(&model, &template);

    assert!(documents.code_review.contains("## 2. Executive Summary"));
    assert!(
        documents
            .code_review
            .contains("could not load the authorship artifact"),
        "{}",
        documents.code_review
    );
}

// ─── Closing signature (#6082 lap 7) ─────────────────────────────────────────

/// A rendered document shaped like the graded report: the template's signature,
/// then the sections the reporter appends after it.
const SIGNED_THEN_APPENDED: &str = "## 9. Gaps & Caveats\n\n- a caveat\n\n---\n\
*Generated by trusty-review report analysis — template report-technical-dd v0.1*\n\
*Source: manifest.toml*\n\n\n## Synthesis Status\n\n- synthesis: available\n\n\n\
## Investigation Coverage\n\n- investigation: available\n";

/// The blocking defect: the signature signed off three sections early.
///
/// Fails before the fix — nothing moved it, so Investigation Coverage rendered
/// after the line saying the report was generated.
#[test]
fn signature_moves_below_appended_sections() {
    let out = signature_last(SIGNED_THEN_APPENDED);
    let last = out
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .expect("a last line");
    assert_eq!(last, "*Source: manifest.toml*");
    assert!(
        out.find("## Investigation Coverage").expect("coverage")
            < out.find(SIGNATURE_NEEDLE).expect("signature"),
        "the appended sections must precede the signature:\n{out}"
    );
    // The rule travels with the block, and only one of each survives.
    assert_eq!(out.matches("\n---\n").count(), 1);
    assert_eq!(out.matches(SIGNATURE_NEEDLE).count(), 1);
}

/// Running the pass twice changes nothing, so it is safe on any render.
#[test]
fn signature_already_last_is_unchanged() {
    let once = signature_last(SIGNED_THEN_APPENDED);
    assert_eq!(signature_last(&once), once);
}

/// A custom template with no signature is returned byte-identical.
#[test]
fn no_signature_leaves_the_document_whole() {
    let doc = "## 9. Gaps & Caveats\n\n- a caveat\n";
    assert_eq!(signature_last(doc), doc);
}
