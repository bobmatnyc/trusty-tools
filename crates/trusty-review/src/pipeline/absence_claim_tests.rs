//! Unit tests for the diff-absence claim check (#1873).
//!
//! Why: the reported harm is a full BLOCK driven by one finding whose premise
//! the changeset disproves. These pin both directions — the refuted claim is
//! dropped, and a claim about a file the diff really does not touch survives.
//! What: drives `drop_refuted_absence_claims` against a two-file index built
//! the way `citation_check_tests` builds one.
//! Test: this file.

use super::*;
use crate::models::{Effort, Finding};
use crate::pipeline::diff_analyzer::models::{
    FileDisposition, FilteredDiff, FilteredFile, FilteredHunk,
};
use std::collections::HashMap;

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn kept_file(name: &str, lines: &[&str]) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::Kept,
        hunks: vec![FilteredHunk {
            header: "@@ -1,1 +1,2 @@".to_string(),
            lines: lines.iter().map(|l| l.to_string()).collect(),
            substantive_confidence: 1.0,
            reason_kept: "test".to_string(),
        }],
        dropped_hunks: Vec::new(),
        summary_line: None,
    }
}

/// The #1873 changeset shape: a file that imports from a NEW sibling module,
/// plus that sibling — the two chunks a map call never sees together.
fn index_1873() -> DiffContentIndex {
    DiffContentIndex::from_filtered(&FilteredDiff {
        files: vec![
            kept_file(
                "crates/trusty-mpm/src/daemon/doctor.rs",
                &[
                    "+#[path = \"doctor_fs_checks.rs\"]",
                    "+mod doctor_fs_checks;",
                ],
            ),
            kept_file(
                "crates/trusty-mpm/src/daemon/doctor_fs_checks.rs",
                &["+pub fn check_fs() -> bool { true }"],
            ),
        ],
        dropped_files: Vec::new(),
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 0,
        filtered_byte_size: 0,
    })
}

fn finding(file: &str, description: &str) -> Finding {
    Finding::new(
        file,
        "compile-break",
        description,
        "add the module",
        0.55,
        Effort::High,
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// REGRESSION (#1873): the verbatim phantom — a finding calling a file missing
/// from the diff when the diff adds it — is dropped.
#[test]
fn refutes_a_missing_file_claim_when_the_file_is_in_the_diff() {
    let index = index_1873();
    let mut findings = vec![finding(
        "crates/trusty-mpm/src/daemon/doctor.rs",
        "doctor.rs declares `#[path = \"doctor_fs_checks.rs\"] mod doctor_fs_checks;` but \
         `crates/trusty-mpm/src/daemon/doctor_fs_checks.rs` is not present in diff — \
         possible compile break.",
    )];

    let dropped = drop_refuted_absence_claims(&mut findings, &index);

    assert_eq!(
        dropped, 1,
        "the changeset adds the file the claim calls absent"
    );
    assert!(
        findings.is_empty(),
        "a finding resting on a refuted premise must not reach the verdict floor"
    );
}

/// A claim about a file the changeset genuinely does not touch survives — this
/// check refutes, it never confirms.
#[test]
fn keeps_a_missing_file_claim_for_a_file_the_diff_does_not_touch() {
    let index = index_1873();
    let mut findings = vec![finding(
        "crates/trusty-mpm/src/daemon/doctor.rs",
        "`crates/trusty-mpm/src/daemon/doctor_net_checks.rs` is not present in diff.",
    )];

    let dropped = drop_refuted_absence_claims(&mut findings, &index);

    assert_eq!(dropped, 0, "nothing in the changeset refutes this claim");
    assert_eq!(findings.len(), 1);
}

/// An ordinary finding with no absence claim is untouched.
#[test]
fn keeps_an_ordinary_finding() {
    let index = index_1873();
    let mut findings = vec![finding(
        "crates/trusty-mpm/src/daemon/doctor.rs",
        "check_fs() returns true unconditionally, so the doctor never reports a \
         filesystem failure.",
    )];

    let dropped = drop_refuted_absence_claims(&mut findings, &index);

    assert_eq!(dropped, 0);
    assert_eq!(findings.len(), 1);
}

/// A claim naming no path is judged against the finding's own `file`.
#[test]
fn refutes_using_the_findings_own_file_when_the_sentence_names_no_path() {
    let index = index_1873();
    let mut findings = vec![finding(
        "crates/trusty-mpm/src/daemon/doctor_fs_checks.rs",
        "The referenced module is not present in the diff, so this will not compile.",
    )];

    let dropped = drop_refuted_absence_claims(&mut findings, &index);

    assert_eq!(dropped, 1);
    assert!(findings.is_empty());
}

/// A present path in a DIFFERENT sentence does not refute an absence claim
/// about another file.
#[test]
fn ignores_a_present_path_in_a_different_sentence() {
    let index = index_1873();
    let mut findings = vec![finding(
        "crates/trusty-mpm/src/daemon/doctor.rs",
        "`crates/trusty-mpm/src/daemon/doctor.rs` grew a module declaration. \
         `crates/trusty-mpm/src/daemon/doctor_net_checks.rs` is not present in diff.",
    )];

    let dropped = drop_refuted_absence_claims(&mut findings, &index);

    assert_eq!(
        dropped, 0,
        "the present path is in a sentence that makes no absence claim"
    );
    assert_eq!(findings.len(), 1);
}

/// Every marker phrase is recognised, so a rewording of the same claim is not
/// a way past the check.
#[test]
fn matches_each_absence_marker() {
    let index = index_1873();
    for marker in ABSENCE_MARKERS {
        let mut findings = vec![finding(
            "crates/trusty-mpm/src/daemon/doctor.rs",
            &format!(
                "`crates/trusty-mpm/src/daemon/doctor_fs_checks.rs` {marker} — compile break."
            ),
        )];
        let dropped = drop_refuted_absence_claims(&mut findings, &index);
        assert_eq!(dropped, 1, "marker not recognised: {marker:?}");
    }
}

/// Path extraction reads a backticked path, a bare one, and one wearing
/// trailing punctuation — and reads prose as prose.
#[test]
fn path_candidates_finds_backticked_and_bare_paths() {
    let found = path_candidates("`src/a.rs` and src/b.rs, plus lib.rs — but not this.");
    assert_eq!(found, vec!["src/a.rs", "src/b.rs", "lib.rs"]);
    assert!(
        path_candidates("the module is gone, i.e. removed").is_empty(),
        "prose with a dotted abbreviation is not a path"
    );
}
