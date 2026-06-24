//! Unit tests for `mapreduce::outcome` types.
//!
//! Why: split from `outcome.rs` to honour the 500-line cap and keep the type
//! definitions readable.
//! What: covers `MapOutcome::file`, `MapReduceStats::is_partial`, and the
//! default-construction invariants.
//! Test: this is the test module.

use super::*;
use crate::models::{Effort, Finding};

fn finding(file: &str, desc: &str) -> Finding {
    Finding::new(file, "k", desc, "", 0.9, Effort::Low)
}

#[test]
fn map_outcome_file_accessor() {
    let reviewed = MapOutcome::Reviewed {
        file: "src/a.rs".to_string(),
        verdict: Verdict::Approve,
        findings: vec![finding("src/a.rs", "x")],
    };
    assert_eq!(reviewed.file(), "src/a.rs");

    let skipped = MapOutcome::Skipped {
        file: "src/b.rs".to_string(),
        note: "deleted file".to_string(),
    };
    assert_eq!(skipped.file(), "src/b.rs");

    let failed = MapOutcome::Failed {
        file: "src/c.rs".to_string(),
        error: "boom".to_string(),
        hunk_oversized: true,
    };
    assert_eq!(failed.file(), "src/c.rs");
}

#[test]
fn stats_default_is_not_partial() {
    let s = MapReduceStats::default();
    assert!(!s.is_partial(), "an all-zero stats block is not partial");
}

#[test]
fn stats_partial_on_failed() {
    let s = MapReduceStats {
        files_failed: 1,
        ..Default::default()
    };
    assert!(s.is_partial(), "a failed file marks coverage partial");
}

#[test]
fn stats_partial_on_oversized_hunk() {
    let s = MapReduceStats {
        hunks_oversized: 1,
        ..Default::default()
    };
    assert!(s.is_partial(), "an over-cap hunk marks coverage partial");
}
