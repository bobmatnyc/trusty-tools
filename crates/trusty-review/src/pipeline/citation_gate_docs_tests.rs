//! Tests for the `[doc:]` citation (#9193).

use std::collections::HashMap;

use super::*;
use crate::models::Effort;
use crate::pipeline::citation_check::{DiffContentIndex, enforce_citation_integrity};
use crate::pipeline::citation_gate::{LineIndex, enforce_line_citations};
use crate::pipeline::diff_analyzer::models::{
    FileDisposition, FilteredDiff, FilteredFile, FilteredHunk,
};

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const ADR: &str = "docs/adr/0001-totals.md";
const ADR_TEXT: &str = "# Totals\n\nInvoice totals must use checked_add so a sum never wraps.\n";
/// A code quote of line 1 of the fixture diff.
const QUOTE: &str = "`amounts.iter().sum::<u64>()` can overflow";

fn filtered() -> FilteredDiff {
    FilteredDiff {
        files: vec![FilteredFile {
            filename: "src/billing.rs".to_string(),
            status: "added".to_string(),
            disposition: FileDisposition::Kept,
            hunks: vec![FilteredHunk {
                header: "@@ -0,0 +1,1 @@".to_string(),
                lines: vec!["+    let total = amounts.iter().sum::<u64>();".to_string()],
                substantive_confidence: 1.0,
                reason_kept: "test".to_string(),
            }],
            dropped_hunks: Vec::new(),
            summary_line: None,
        }],
        dropped_files: Vec::new(),
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 0,
        filtered_byte_size: 0,
    }
}

fn corpus() -> DocCorpus {
    let mut c = DocCorpus::at(HEAD);
    c.insert(ADR, ADR_TEXT);
    c
}

fn finding(body: &str) -> Finding {
    let mut f = Finding::new(
        "src/billing.rs",
        "overflow",
        body,
        "use checked_add",
        0.9,
        Effort::High,
    );
    f.line = Some(1);
    f
}

/// How many of one finding with `body` survive the gate over `docs`.
fn kept(body: &str, docs: &DocCorpus) -> usize {
    let mut findings = vec![finding(body)];
    enforce_line_citations(
        &mut findings,
        &LineIndex::from_filtered(&filtered()).with_docs(docs),
    );
    findings.len()
}

fn cite(path: &str, sha: &str, excerpt: &str) -> String {
    format!("{QUOTE} [doc: {path}@{sha} — \"{excerpt}\"]")
}

/// #9193 criterion 5: an exact excerpt of a doc read at the head resolves,
/// with the full SHA or a 7-character prefix, and any separator.
#[test]
fn doc_citation_with_exact_excerpt_at_head_is_kept() {
    let excerpt = "Invoice totals must use checked_add";
    assert_eq!(kept(&cite(ADR, HEAD, excerpt), &corpus()), 1);
    assert_eq!(kept(&cite(ADR, &HEAD[..7], excerpt), &corpus()), 1);
    let upper = format!(
        "{QUOTE} [doc: {ADR}@{} - \"{excerpt}\"]",
        HEAD[..9].to_uppercase()
    );
    assert_eq!(kept(&upper, &corpus()), 1);
    let spaced =
        format!("{QUOTE} [doc: {ADR}@{HEAD} – \"Invoice  totals\n must use checked_add\"]");
    assert_eq!(kept(&spaced, &corpus()), 1, "whitespace is normalized");
}

/// #9193 criterion 5: an excerpt the doc does not hold is withheld.
#[test]
fn doc_citation_with_fabricated_excerpt_is_withheld() {
    let body = cite(ADR, HEAD, "Invoice totals may wrap on overflow");
    assert_eq!(kept(&body, &corpus()), 0);
    assert_eq!(
        check_doc_citations(&finding(&body), &corpus()).map(|(r, _)| r),
        Some(DOC_EXCERPT_ABSENT)
    );
}

/// #9193 criterion 5: text past the cap is not in the corpus, so a quote of
/// the cut tail is withheld though the file holds it.
#[test]
fn excerpt_from_the_cut_tail_is_withheld() {
    let full = format!("{}TAIL_SENTENCE only past the cap", "x ".repeat(20));
    let kept_text: String = full.chars().take(40).collect();
    let mut docs = DocCorpus::at(HEAD);
    docs.insert(ADR, &kept_text);
    assert_eq!(
        kept(&cite(ADR, HEAD, "TAIL_SENTENCE only past the cap"), &docs),
        0
    );
}

/// #9193: one doc's text never verifies a citation of another doc.
#[test]
fn excerpt_of_doc_a_cited_as_doc_b_is_withheld() {
    let mut docs = corpus();
    docs.insert(
        "docs/specs/b.md",
        "Spec B says nothing about totals at all.",
    );
    let body = cite(
        "docs/specs/b.md",
        HEAD,
        "Invoice totals must use checked_add",
    );
    assert_eq!(kept(&body, &docs), 0);
}

/// A SHA that is not a prefix of the head's is withheld.
#[test]
fn doc_citation_with_wrong_sha_is_withheld() {
    let excerpt = "Invoice totals must use checked_add";
    assert_eq!(kept(&cite(ADR, "fedcba9876543210", excerpt), &corpus()), 0);
}

/// A path that was never read at the head is withheld.
#[test]
fn doc_citation_to_an_unfetched_path_is_withheld() {
    let excerpt = "Invoice totals must use checked_add";
    assert_eq!(
        kept(&cite("docs/adr/0002-other.md", HEAD, excerpt), &corpus()),
        0
    );
}

/// With `spec_docs` off the corpus is empty, so any `[doc:]` is withheld.
#[test]
fn doc_citation_without_spec_docs_is_withheld() {
    let body = cite(ADR, HEAD, "Invoice totals must use checked_add");
    assert_eq!(kept(&body, &DocCorpus::default()), 0);
    assert_eq!(
        kept(QUOTE, &DocCorpus::default()),
        1,
        "control: no [doc:] citation"
    );
}

/// An excerpt under the quote floor cannot verify a citation.
#[test]
fn short_excerpt_is_withheld() {
    let body = cite(ADR, HEAD, "checked_add");
    assert_eq!(
        check_doc_citations(&finding(&body), &corpus()).map(|(r, _)| r),
        Some(DOC_EXCERPT_SHORT)
    );
}

/// #9193 amendment 3: a `[doc:]` with no quoted excerpt is withheld.
#[test]
fn doc_citation_without_excerpt_is_withheld() {
    for body in [
        format!("{QUOTE} [doc: {ADR}@{HEAD}]"),
        format!("{QUOTE} [doc: {ADR}@{HEAD} — Invoice totals must use checked_add]"),
    ] {
        assert_eq!(kept(&body, &corpus()), 0, "{body}");
    }
}

/// #9193 amendment 3: a citation missing its `@sha`, its path, or its
/// separator shape is malformed and withheld.
#[test]
fn doc_citation_missing_sha_or_malformed_is_withheld() {
    let excerpt = "Invoice totals must use checked_add";
    for body in [
        format!("{QUOTE} [doc: {ADR} — \"{excerpt}\"]"),
        format!("{QUOTE} [doc: @{HEAD} — \"{excerpt}\"]"),
        format!("{QUOTE} [doc: {ADR}@ — \"{excerpt}\"]"),
        format!("{QUOTE} [doc: {ADR}@{HEAD}@ — \"{excerpt}\"]"),
        format!("{QUOTE} [doc: {ADR} {HEAD} — \"{excerpt}\"]"),
        format!("{QUOTE} [doc: \"{excerpt}\"]"),
    ] {
        assert_eq!(kept(&body, &corpus()), 0, "{body}");
        assert!(
            check_doc_citations(&finding(&body), &corpus()).is_some(),
            "{body}"
        );
    }
}

/// #9193 amendment 3: a 6-character or empty SHA prefix never resolves.
#[test]
fn six_char_and_empty_sha_prefix_is_withheld() {
    let excerpt = "Invoice totals must use checked_add";
    assert_eq!(kept(&cite(ADR, &HEAD[..6], excerpt), &corpus()), 0);
    assert_eq!(kept(&cite(ADR, "", excerpt), &corpus()), 0);
    assert_eq!(kept(&cite(ADR, "zzzzzzz", excerpt), &corpus()), 0);
    assert_eq!(
        kept(&cite(ADR, &HEAD[..7], excerpt), &corpus()),
        1,
        "control"
    );
}

/// #9193 amendment 3: a `]` inside the excerpt ends the bracket early; the
/// cut fragment is never checked, even when the doc holds it.
#[test]
fn doc_excerpt_containing_bracket_is_withheld() {
    let mut docs = DocCorpus::at(HEAD);
    docs.insert(ADR, "Rule: the totals in section [1] must use checked_add.");
    let body = cite(ADR, HEAD, "the totals in section [1] must use checked_add");
    assert_eq!(kept(&body, &docs), 0);
}

/// #9193: a `[doc:]` excerpt is not scanned as a generic quote, so the
/// pre-gate citation check keeps a finding whose only long quote is in it.
#[test]
fn doc_citation_is_not_scanned_as_a_generic_quote() {
    let body = "`sum::<u64>` wraps [doc: docs/adr/0001-totals.md@0123456 — \"Invoice totals must use checked_add\"]";
    let mut findings = vec![finding(body)];
    let mut withheld = Vec::new();
    let index = DiffContentIndex::from_filtered(&filtered());
    assert_eq!(
        enforce_citation_integrity(&mut findings, &index, &mut withheld),
        0
    );
    assert_eq!(findings.len(), 1, "{withheld:?}");
}
