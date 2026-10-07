//! Tests for doc caps, sections and ledger rows (#9193).

use super::*;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn text(path: &str, body: &str) -> DocRead {
    DocRead {
        path: path.to_string(),
        read: Read::Text(body.to_string()),
        self_edited: false,
    }
}

fn render(reads: &[DocRead]) -> (String, ContextSourceRecord, DocCorpus) {
    let mut corpus = DocCorpus::at(SHA);
    let (section, row) = render_kind(&SPEC_DOCS, SHA, reads, 0, None, &mut corpus);
    (section, row, corpus)
}

/// #9193 criterion 2: an over-cap doc is cut at 16,000 characters with a
/// visible marker and recorded `truncated`; only the kept text is citable.
#[test]
fn doc_over_cap_is_cut_with_marker_and_recorded_truncated() {
    let body = format!("{}TAIL", "a".repeat(MAX_SPEC_DOC_CHARS - 3));
    let (section, row, corpus) = render(&[text("docs/adr/x.md", &body)]);
    assert!(section.contains(
        "[... truncated: 1 more characters omitted; docs/adr/x.md is capped at 16000 characters"
    ));
    assert!(!section.contains("TAIL"));
    assert_eq!(row.state, SourceState::Truncated);
    let item = &row.items[0];
    assert_eq!(
        (item.state, item.chars, item.chars_omitted),
        (SourceState::Truncated, 16_000, 1)
    );
    assert_eq!(corpus, {
        let mut c = DocCorpus::at(SHA);
        c.insert("docs/adr/x.md", &body[..MAX_SPEC_DOC_CHARS]);
        c
    });
}

/// The first doc past the 48,000-character section cap is omitted whole,
/// with a detail, and is not citable.
#[test]
fn section_cap_omits_whole_doc_with_detail() {
    let big = "b".repeat(MAX_SPEC_DOC_CHARS);
    let reads: Vec<DocRead> = (0..4)
        .map(|i| text(&format!("docs/adr/{i}.md"), &big))
        .collect();
    let (section, row, corpus) = render(&reads);
    assert_eq!(section.matches("### docs/adr/").count(), 3);
    let last = &row.items[3];
    assert_eq!(last.state, SourceState::Omitted);
    assert_eq!(last.chars_omitted, MAX_SPEC_DOC_CHARS);
    assert_eq!(
        last.detail.as_deref(),
        Some("over the 48000-character section cap")
    );
    let mut three = DocCorpus::at(SHA);
    (0..3).for_each(|i| three.insert(&format!("docs/adr/{i}.md"), &big));
    assert_eq!(corpus, three);
}

/// At most six docs are rendered; the seventh is omitted with a detail.
#[test]
fn seven_docs_keep_six_omit_tail() {
    let reads: Vec<DocRead> = (0..7)
        .map(|i| text(&format!("docs/adr/{i}.md"), "A short ADR body."))
        .collect();
    let (section, row, _) = render(&reads);
    assert_eq!(section.matches("### docs/adr/").count(), 6);
    assert_eq!(row.items[6].state, SourceState::Omitted);
    assert_eq!(row.items[6].detail.as_deref(), Some("over the 6-doc limit"));
    assert_eq!(row.state, SourceState::Truncated);
}

/// #9193 amendment 7: the row is the worst of its items; one failed read
/// among good ones marks the row `unavailable`, and the good docs render.
#[test]
fn one_doc_500_among_good_docs_marks_row_unavailable() {
    let reads = [
        text("docs/adr/a.md", "First ADR body."),
        DocRead {
            path: "docs/adr/b.md".to_string(),
            read: Read::Unavailable("GitHub API returned 500: oops".to_string()),
            self_edited: false,
        },
        text("docs/adr/c.md", "Third ADR body."),
    ];
    let (section, row, _) = render(&reads);
    assert_eq!(row.state, SourceState::Unavailable);
    assert_eq!(section.matches("### docs/adr/").count(), 2);
    assert_eq!(
        row.items[1].detail.as_deref(),
        Some("GitHub API returned 500: oops")
    );
}

/// #9193 amendment 14: the section teaches the `[doc:]` form beside the four
/// system-prompt forms, and names each doc's `path@sha` in its heading.
#[test]
fn section_note_teaches_the_doc_form() {
    let (section, _, _) = render(&[text("docs/adr/a.md", "ADR body.")]);
    assert!(section.starts_with("## Referenced docs\n\n"));
    assert!(section.contains("in addition to the four forms above"));
    assert!(section.contains("[doc: path@sha — \"exact excerpt\"]"));
    assert!(section.contains(&format!("### docs/adr/a.md@{SHA}\n")));
}

/// Nothing read renders nothing; a row with no candidates says why.
#[test]
fn no_doc_read_renders_no_section() {
    let absent = DocRead {
        path: "docs/adr/gone.md".to_string(),
        read: Read::Absent("not found at 0123456".to_string()),
        self_edited: false,
    };
    let (section, row, corpus) = render(&[absent, text("docs/adr/blank.md", "  \n")]);
    assert!(section.is_empty() && corpus.is_empty());
    assert_eq!(row.state, SourceState::Absent);
    let (section, row, _) = render(&[]);
    assert!(section.is_empty());
    assert_eq!(row.state, SourceState::Absent);
    assert!(row.detail.is_some());
}

/// Reads past the budget and a failed discovery are recorded, never silent.
#[test]
fn skipped_reads_and_discovery_failure_are_items() {
    let mut corpus = DocCorpus::at(SHA);
    let reads = [text("CLAUDE.md", "Use thiserror.")];
    let (section, row) = render_kind(
        &CLAUDE_MD,
        SHA,
        &reads,
        2,
        Some("trusty-search failed: down"),
        &mut corpus,
    );
    assert!(section.starts_with("## Repository conventions (CLAUDE.md)\n\n"));
    let ids: Vec<_> = row.items.iter().map(|i| (i.id.as_str(), i.state)).collect();
    assert_eq!(
        ids,
        [
            ("CLAUDE.md", SourceState::Used),
            ("(rest)", SourceState::Omitted),
            ("discovery", SourceState::Unavailable)
        ]
    );
    assert_eq!(row.state, SourceState::Unavailable);
}
