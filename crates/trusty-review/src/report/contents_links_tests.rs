use super::*;

/// Links every top-level (`## `) heading present, except Executive Summary.
#[test]
fn links_every_top_level_heading() {
    let doc = format!(
        "## 1. Report Metadata\n\n## 2. Executive Summary\n\n{SENTINEL}\n\n### Top Risks\n\n## 3. Code Quality & Architecture\n\ncontent\n\n## 9. Gaps & Caveats\n\nx\n"
    );
    let out = inject(&doc);
    assert!(!out.contains(SENTINEL));
    assert!(out.contains("[1. Report Metadata](#1-report-metadata)"));
    assert!(out.contains("[3. Code Quality & Architecture](#3-code-quality--architecture)"));
    assert!(out.contains("[9. Gaps & Caveats](#9-gaps--caveats)"));
    // Never a self-link to the section the list lives inside.
    assert!(!out.contains("[2. Executive Summary]"));
}

/// A heading absent from the document never gets a link — the anchor set is
/// derived from the ACTUAL rendered headings, not a fixed candidate list.
#[test]
fn never_links_a_heading_absent_from_the_document() {
    let doc = format!("## 2. Executive Summary\n\n{SENTINEL}\n\n## 9. Gaps & Caveats\n\nx\n");
    let out = inject(&doc);
    assert!(out.contains("[9. Gaps & Caveats]"));
    assert!(!out.contains("Security Posture"));
    assert!(!out.contains("Performance"));
}

/// A document with no sentinel (a custom template without the placeholder)
/// is returned unchanged.
#[test]
fn noop_without_sentinel() {
    let doc = "## 1. Report Metadata\n\ncontent\n";
    assert_eq!(inject(doc), doc);
}

/// A document whose only heading is Executive Summary itself produces an
/// empty jump-list block rather than a lone "**Contents**" with nothing
/// under it.
#[test]
fn no_other_headings_yields_empty_block() {
    let doc = format!("## 2. Executive Summary\n\n{SENTINEL}\n\ntext\n");
    let out = inject(&doc);
    assert!(!out.contains(SENTINEL));
    assert!(!out.contains("**Contents**"));
}

/// Two headings that would slug identically get distinct anchors.
#[test]
fn slug_collision_gets_a_distinct_anchor() {
    let doc = format!("## 2. Executive Summary\n\n{SENTINEL}\n\n## Notes\n\na\n\n## Notes!\n\nb\n");
    let out = inject(&doc);
    assert!(out.contains("(#notes)"));
    assert!(out.contains("(#notes-2)"));
}

/// A `## `-prefixed line inside a fenced evidence quote (raw_evidence embeds
/// source bytes verbatim) must never be mistaken for a document heading — it
/// is quoted source text, not a section boundary, so it must produce no
/// Contents link and no dangling anchor.
#[test]
fn fenced_evidence_hash_line_is_not_mistaken_for_a_heading() {
    let doc = format!(
        "## 2. Executive Summary\n\n{SENTINEL}\n\n## 3. Code Quality & Architecture\n\n```\n## Not A Real Section\n```\n\n## 9. Gaps & Caveats\n\nx\n"
    );
    let out = inject(&doc);
    assert!(!out.contains(SENTINEL));
    assert!(out.contains("[3. Code Quality & Architecture](#3-code-quality--architecture)"));
    assert!(out.contains("[9. Gaps & Caveats](#9-gaps--caveats)"));
    // The fenced quote's own text survives untouched, but it must never be
    // linked as a heading (no bullet, no anchor).
    assert!(!out.contains("[Not A Real Section]"));
    assert!(!out.contains("(#not-a-real-section)"));
}

/// #6137: GitHub DELETES punctuation and then maps spaces to dashes, so a
/// heading with an `&` anchors with TWO dashes. `manifest::slugify` collapsed
/// the whole run to one, and four of a real report's twelve jump-list links
/// pointed at anchors that did not exist.
#[test]
fn anchor_matches_github_for_ampersand_headings() {
    assert_eq!(
        github_anchor("Code Quality & Architecture"),
        "code-quality--architecture"
    );
    assert_eq!(
        github_anchor("Performance & Scalability"),
        "performance--scalability"
    );
    assert_eq!(
        github_anchor("8. Ticketing & Delivery Traceability"),
        "8-ticketing--delivery-traceability"
    );
}

/// A numbered heading's period is deleted, not turned into a dash — the same
/// answer `slugify` gave, and it must stay that way.
#[test]
fn anchor_matches_github_for_numbered_headings() {
    assert_eq!(github_anchor("1. Report Metadata"), "1-report-metadata");
    assert_eq!(github_anchor("Key Facts"), "key-facts");
    assert_eq!(github_anchor("Security Posture"), "security-posture");
}
