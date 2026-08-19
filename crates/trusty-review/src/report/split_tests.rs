//! Unit tests for the code-review / authorship output split (#6046).

use super::*;

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
    let doc = authorship_document("Acme DD", "2026-08-19", &section.expect("section"));

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
    let doc = authorship_document("Acme DD", "2026-08-19", &section.expect("section"));

    assert!(doc.contains("Bus factor 2."), "{doc}");
    assert!(doc.contains("| acme | 7 |"), "{doc}");
    assert!(
        !doc.contains("## Authorship & Key-Person Risk"),
        "the heading is promoted to the title, not repeated:\n{doc}"
    );
    assert!(doc.ends_with('\n'), "must end with a newline");
}
