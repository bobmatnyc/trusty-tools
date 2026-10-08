//! Tests for `extract_issue_refs`: one test per example row group of #9197.
//!
//! Why: each row pins a rule that a plausible wrong implementation breaks — an
//! any-`#N` scan, JIRA-pattern reuse, a regex over unmasked text, an unordered
//! set, or a loose digit match.
//! What: plain inputs against expected `IssueRef` lists; no I/O.
//! Test: this file is the test surface; `cargo test -p trusty-common
//! --features intent-source issue_refs` runs it.

use super::issue_refs::{IssueRef, extract_issue_refs};

/// Bare `#N` refs, in the given order.
fn bare(numbers: &[u64]) -> Vec<IssueRef> {
    numbers
        .iter()
        .map(|&number| IssueRef {
            number,
            owner_repo: None,
        })
        .collect()
}

#[test]
fn issue_refs_closes_single() {
    assert_eq!(extract_issue_refs("Closes #42"), bare(&[42]));
}

#[test]
fn issue_refs_list_separators() {
    assert_eq!(
        extract_issue_refs("closes #42, #43 and #44"),
        bare(&[42, 43, 44])
    );
    assert_eq!(
        extract_issue_refs("Fixes #1 & #2, and #3"),
        bare(&[1, 2, 3])
    );
}

#[test]
fn issue_refs_refs_part_of_see() {
    assert_eq!(extract_issue_refs("Refs #9192"), bare(&[9192]));
    assert_eq!(extract_issue_refs("Part of #10"), bare(&[10]));
    assert_eq!(extract_issue_refs("See #7"), bare(&[7]));
}

#[test]
fn issue_refs_case_and_colon() {
    assert_eq!(extract_issue_refs("FIXES #3"), bare(&[3]));
    assert_eq!(extract_issue_refs("Closes: #3"), bare(&[3]));
}

#[test]
fn issue_refs_owner_repo_form() {
    assert_eq!(
        extract_issue_refs("Fixes owner/repo#5"),
        vec![IssueRef {
            number: 5,
            owner_repo: Some(("owner".to_string(), "repo".to_string())),
        }]
    );
}

#[test]
fn issue_refs_dedupe_in_order() {
    assert_eq!(
        extract_issue_refs("Closes #1\nRefs #1\nrefs #2"),
        bare(&[1, 2])
    );
    // Descending first occurrences: a sorted or hashed set reorders these.
    assert_eq!(
        extract_issue_refs("Closes #9\nRefs #3\nrefs #9\nSee #1"),
        bare(&[9, 3, 1])
    );
}

#[test]
fn issue_refs_passing_mention_does_not_link() {
    assert_eq!(
        extract_issue_refs("See discussion in #42 for background"),
        bare(&[])
    );
}

#[test]
fn issue_refs_bare_mention_does_not_link() {
    assert_eq!(extract_issue_refs("Mentioned #42 in passing"), bare(&[]));
}

#[test]
fn issue_refs_adr_id_does_not_link() {
    assert_eq!(extract_issue_refs("Refs ADR-0043"), bare(&[]));
}

#[test]
fn issue_refs_adr_then_closes() {
    assert_eq!(
        extract_issue_refs("Part of ADR-0043. Closes #42"),
        bare(&[42])
    );
}

#[test]
fn issue_refs_azure_and_jira_ids() {
    assert_eq!(extract_issue_refs("Closes AB#12"), bare(&[]));
    assert_eq!(extract_issue_refs("Closes PROJ-12"), bare(&[]));
}

#[test]
fn issue_refs_malformed_numbers() {
    assert_eq!(extract_issue_refs("Closes #42abc"), bare(&[]));
    assert_eq!(extract_issue_refs("Closes #0"), bare(&[]));
    assert_eq!(extract_issue_refs("Closes #"), bare(&[]));
    // 19 digits: over the 1-18 digit limit, though it still fits a u64.
    assert_eq!(extract_issue_refs("Closes #1234567890123456789"), bare(&[]));
}

#[test]
fn issue_refs_code_and_comments_ignored() {
    assert_eq!(extract_issue_refs("`Closes #1`"), bare(&[]));
    assert_eq!(extract_issue_refs("<!-- Closes #1 -->"), bare(&[]));
    assert_eq!(extract_issue_refs("```\nCloses #1\n```"), bare(&[]));
    assert_eq!(extract_issue_refs("~~~text\nCloses #1\n~~~"), bare(&[]));
    assert_eq!(extract_issue_refs("<!--\nCloses #1\n-->"), bare(&[]));
    // Text after a closed comment or fence still links.
    assert_eq!(
        extract_issue_refs("<!-- Closes #1 --> Closes #2\n```\nCloses #3\n```\nRefs #4"),
        bare(&[2, 4])
    );
}

#[test]
fn issue_refs_keyword_and_ref_split_across_lines() {
    assert_eq!(extract_issue_refs("Closes\n#3"), bare(&[]));
}

#[test]
fn issue_refs_empty_text() {
    assert_eq!(extract_issue_refs(""), bare(&[]));
}
