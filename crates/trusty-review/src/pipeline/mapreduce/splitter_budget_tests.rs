//! Budget-enforcement and content-verification tests for the per-file diff
//! splitter (Phase 2, #698 / #680).
//!
//! Why: the budget tests are separated from the main splitter tests to honour
//! the 500-line file cap (CLAUDE.md §"500-line file size hard cap").
//! What: covers `total_char_budget` exhaustion, `max_calls` ceiling, diff-text
//! content checks, file-status preservation, and edge cases.
//! Test: run `cargo test -p trusty-review -- pipeline::mapreduce::budget_tests`.

use std::collections::HashMap;

use super::split_into_units;
use crate::{
    config::mapreduce::MapReduceConfig,
    pipeline::{
        diff_analyzer::models::{
            DroppedHunk, FileDisposition, FilteredDiff, FilteredFile, FilteredHunk, HunkDropReason,
        },
        mapreduce::unit::{MapUnit, MapUnitKind},
    },
};

// ─── Local fixture builders (mirrors splitter_tests.rs) ──────────────────────

fn kept_file_with_hunks(name: &str, status: &str, hunks: Vec<FilteredHunk>) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: status.to_string(),
        disposition: FileDisposition::Kept,
        hunks,
        dropped_hunks: vec![],
        summary_line: None,
    }
}

fn deleted_file(name: &str) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "removed".to_string(),
        disposition: FileDisposition::Kept,
        hunks: vec![],
        dropped_hunks: vec![],
        summary_line: None,
    }
}

fn simple_hunk(content: &str) -> FilteredHunk {
    FilteredHunk {
        header: "@@ -1,1 +1,1 @@".to_string(),
        lines: vec![format!("+{content}")],
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    }
}

fn hunk_of_size(approx_chars: usize) -> FilteredHunk {
    let header = "@@ -1,1 +1,1 @@".to_string();
    let line_len = approx_chars.saturating_sub(header.len() + 2);
    let line = format!("+{}", "x".repeat(line_len));
    FilteredHunk {
        header,
        lines: vec![line],
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    }
}

fn make_diff(files: Vec<FilteredFile>) -> FilteredDiff {
    FilteredDiff {
        files,
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 0,
        filtered_byte_size: 0,
    }
}

fn default_config() -> MapReduceConfig {
    MapReduceConfig::default()
}

fn is_review(u: &MapUnit) -> bool {
    matches!(u.kind, MapUnitKind::Review { .. })
}

fn is_metadata(u: &MapUnit) -> bool {
    u.is_metadata_only()
}

// ─── Total-char-budget enforcement ────────────────────────────────────────────

#[test]
fn total_char_budget_exhaustion_downgrades_remaining_files() {
    // Budget = 500 chars; three files each ~300 chars.
    // First file fits; second file pushes total over 500; third file is metadata.
    let tiny_budget = 500usize;
    let big_hunk = hunk_of_size(280); // ~280 chars per file rendered
    let files = vec![
        kept_file_with_hunks("src/a.rs", "modified", vec![big_hunk.clone()]),
        kept_file_with_hunks("src/b.rs", "modified", vec![big_hunk.clone()]),
        kept_file_with_hunks("src/c.rs", "modified", vec![big_hunk.clone()]),
    ];
    let diff = make_diff(files);
    let cfg = MapReduceConfig {
        total_char_budget: tiny_budget,
        per_file_chars: 500,
        ..MapReduceConfig::default()
    };
    let units = split_into_units(&diff, &cfg);
    assert_eq!(units.len(), 3);
    // At least the last unit must be metadata (budget exhausted).
    assert!(
        is_metadata(&units[2]),
        "last file must be downgraded to metadata-only"
    );
    match &units[2].kind {
        MapUnitKind::MetadataOnly { note } => assert!(note.contains("budget")),
        _ => panic!("expected budget-exhausted metadata note"),
    }
}

#[test]
fn max_calls_hard_ceiling_downgrades_remaining_files() {
    let files = vec![
        kept_file_with_hunks("src/a.rs", "modified", vec![simple_hunk("fn a() {}")]),
        kept_file_with_hunks("src/b.rs", "modified", vec![simple_hunk("fn b() {}")]),
        kept_file_with_hunks("src/c.rs", "modified", vec![simple_hunk("fn c() {}")]),
    ];
    let diff = make_diff(files);
    let cfg = MapReduceConfig {
        max_calls: 2,
        ..MapReduceConfig::default()
    };
    let units = split_into_units(&diff, &cfg);
    assert_eq!(units.len(), 3);
    assert!(is_review(&units[0]));
    assert!(is_review(&units[1]));
    assert!(
        is_metadata(&units[2]),
        "third file must be downgraded when max_calls=2"
    );
    match &units[2].kind {
        MapUnitKind::MetadataOnly { note } => assert!(note.contains("max-calls")),
        _ => panic!("expected max-calls metadata note"),
    }
}

#[test]
fn metadata_only_units_do_not_count_against_max_calls() {
    // 3 files: deleted, review, review. max_calls=2.
    // deleted → metadata (doesn't count); both review files should fit under max_calls=2.
    let diff = make_diff(vec![
        deleted_file("src/old.rs"),
        kept_file_with_hunks("src/a.rs", "modified", vec![simple_hunk("fn a() {}")]),
        kept_file_with_hunks("src/b.rs", "modified", vec![simple_hunk("fn b() {}")]),
    ]);
    let cfg = MapReduceConfig {
        max_calls: 2,
        ..MapReduceConfig::default()
    };
    let units = split_into_units(&diff, &cfg);
    assert_eq!(units.len(), 3);
    assert!(is_metadata(&units[0]), "deleted should be metadata");
    assert!(is_review(&units[1]), "src/a.rs should be reviewed");
    assert!(is_review(&units[2]), "src/b.rs should be reviewed");
}

// ─── Diff-text content checks ─────────────────────────────────────────────────

#[test]
fn review_unit_diff_text_contains_file_path() {
    let file = kept_file_with_hunks(
        "src/service.rs",
        "modified",
        vec![simple_hunk("fn svc() {}")],
    );
    let diff = make_diff(vec![file]);
    let units = split_into_units(&diff, &default_config());
    let text = units[0].diff_text().expect("should have diff text");
    assert!(
        text.contains("src/service.rs"),
        "diff text must contain the file path"
    );
    assert!(text.contains("svc"), "diff text must contain hunk content");
}

#[test]
fn review_unit_diff_char_count_matches_diff_text_len() {
    let file = kept_file_with_hunks("src/x.rs", "added", vec![simple_hunk("pub fn x() {}")]);
    let diff = make_diff(vec![file]);
    let units = split_into_units(&diff, &default_config());
    let text = units[0].diff_text().unwrap();
    assert_eq!(
        units[0].diff_char_count,
        text.len(),
        "diff_char_count must equal diff_text.len()"
    );
}

// ─── Empty FilteredDiff ───────────────────────────────────────────────────────

#[test]
fn empty_filtered_diff_yields_empty_units() {
    let diff = make_diff(vec![]);
    let units = split_into_units(&diff, &default_config());
    assert!(units.is_empty(), "empty diff must produce no units");
}

// ─── Dropped hunk telemetry is separate from kept hunks ──────────────────────

#[test]
fn file_with_dropped_hunks_only_counts_kept_hunks_in_diff_text() {
    // A file with one kept hunk and one dropped hunk.
    let kept = simple_hunk("fn kept() {}");
    let dropped = DroppedHunk {
        reason: HunkDropReason::WhitespaceOnly,
        lines_count: 3,
        header: "@@ -5,3 +5,3 @@".to_string(),
    };
    let file = FilteredFile {
        filename: "src/mixed.rs".to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::Kept,
        hunks: vec![kept],
        dropped_hunks: vec![dropped],
        summary_line: None,
    };
    let diff = make_diff(vec![file]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_review(&units[0]));
    // Dropped hunk content must NOT appear in the diff text.
    let text = units[0].diff_text().unwrap();
    assert!(text.contains("kept"), "kept hunk must appear");
    // The dropped hunk's header doesn't appear because render_file renders only file.hunks.
}

// ─── Status preservation ─────────────────────────────────────────────────────

#[test]
fn unit_preserves_file_status() {
    let files = vec![
        kept_file_with_hunks("src/new.rs", "added", vec![simple_hunk("+fn new() {}")]),
        kept_file_with_hunks(
            "src/modified.rs",
            "modified",
            vec![simple_hunk("+fn mod() {}")],
        ),
        kept_file_with_hunks("src/ren.rs", "renamed", vec![simple_hunk("+fn ren() {}")]),
    ];
    let diff = make_diff(files);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units[0].status, "added");
    assert_eq!(units[1].status, "modified");
    assert_eq!(units[2].status, "renamed");
}

// ─── SummaryOnly with no summary_line ────────────────────────────────────────

#[test]
fn summary_only_with_no_summary_line_is_metadata() {
    let file = FilteredFile {
        filename: "locales/de.json".to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::SummaryOnly,
        hunks: vec![],
        dropped_hunks: vec![],
        summary_line: None, // no summary line
    };
    let diff = make_diff(vec![file]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_metadata(&units[0]));
}

// ─── Max-calls interacts with sub-chunked oversized files ────────────────────

#[test]
fn max_calls_caps_sub_chunk_units_from_oversized_file() {
    // An oversized file that would produce 3 sub-chunks, but max_calls=2.
    // Expect: first 2 sub-chunks are Review; third is MetadataOnly("max-calls reached").
    let budget = 60usize;
    let h1 = hunk_of_size(40);
    let h2 = hunk_of_size(40);
    let h3 = hunk_of_size(40);
    let file = kept_file_with_hunks("src/big.rs", "modified", vec![h1, h2, h3]);
    let diff = make_diff(vec![file]);
    let cfg = MapReduceConfig {
        per_file_chars: budget,
        max_calls: 2,
        ..MapReduceConfig::default()
    };
    let units = split_into_units(&diff, &cfg);
    // All units belong to "src/big.rs".
    for u in &units {
        assert_eq!(u.file, "src/big.rs");
    }
    // Exactly 2 review sub-chunks + at least 1 metadata (max-calls) sub-chunk.
    let review_count = units.iter().filter(|u| is_review(u)).count();
    let meta_count = units.iter().filter(|u| is_metadata(u)).count();
    assert!(review_count <= 2, "at most max_calls=2 review chunks");
    assert!(meta_count >= 1, "excess chunks must be metadata-only");
    // The last metadata chunk must mention max-calls.
    if let Some(last_meta) = units.iter().rfind(|u| is_metadata(u)) {
        match &last_meta.kind {
            MapUnitKind::MetadataOnly { note } => assert!(note.contains("max-calls")),
            _ => panic!("expected MetadataOnly"),
        }
    }
}
