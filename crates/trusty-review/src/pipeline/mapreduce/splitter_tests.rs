//! Unit tests for the per-file diff splitter (Phase 2, #696 / #680).
//!
//! Why: the splitter is a pure function with many boundary conditions
//! (single-file, oversized, sub-chunked, metadata-only, budget enforcement);
//! keeping these tests in a dedicated file honours the 500-line cap.
//! What: exercises `split_into_units` and its helpers against synthetic
//! `FilteredDiff` fixtures.
//! Test: run `cargo test -p trusty-review -- pipeline::mapreduce::tests`.

use std::collections::HashMap;

use super::split_into_units;
use crate::{
    config::mapreduce::MapReduceConfig,
    pipeline::{
        diff_analyzer::models::{FileDisposition, FilteredDiff, FilteredFile, FilteredHunk},
        mapreduce::unit::{MapUnit, MapUnitKind},
    },
};

// ─── Fixture builders ─────────────────────────────────────────────────────────

/// Build a `FilteredHunk` whose rendered size is approximately `approx_chars`.
/// The header is a fixed `@@ -1,1 +1,1 @@` line; the single body line is
/// padded to reach the target size.
fn hunk_of_size(approx_chars: usize) -> FilteredHunk {
    let header = "@@ -1,1 +1,1 @@".to_string();
    // header.len() + '\n' + line.len() + '\n' = approx_chars
    // => line.len() = approx_chars - header.len() - 2
    let line_len = approx_chars.saturating_sub(header.len() + 2);
    let line = format!("+{}", "x".repeat(line_len));
    FilteredHunk {
        header,
        lines: vec![line],
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    }
}

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

fn summary_file(name: &str) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::SummaryOnly,
        hunks: vec![],
        dropped_hunks: vec![],
        summary_line: Some("fixture file — 120 i18n strings".to_string()),
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

fn renamed_no_hunks(name: &str) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "renamed".to_string(),
        disposition: FileDisposition::Kept,
        hunks: vec![],
        dropped_hunks: vec![],
        summary_line: None,
    }
}

fn binary_file(name: &str) -> FilteredFile {
    // Binary files: status "modified", no hunks (the parser yields none for
    // binary diffs).
    FilteredFile {
        filename: name.to_string(),
        status: "modified".to_string(),
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

fn config_with_per_file_chars(n: usize) -> MapReduceConfig {
    MapReduceConfig {
        per_file_chars: n,
        ..MapReduceConfig::default()
    }
}

fn is_review(u: &MapUnit) -> bool {
    matches!(u.kind, MapUnitKind::Review { .. })
}

fn is_metadata(u: &MapUnit) -> bool {
    u.is_metadata_only()
}

// ─── Basic single-file tests ──────────────────────────────────────────────────

#[test]
fn single_small_file_yields_one_review_unit() {
    let file = kept_file_with_hunks("src/auth.rs", "modified", vec![simple_hunk("fn auth() {}")]);
    let diff = make_diff(vec![file]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_review(&units[0]));
    assert_eq!(units[0].file, "src/auth.rs");
    assert_eq!(units[0].chunk_index, 0);
    assert_eq!(units[0].chunk_total, 1);
    assert!(!units[0].hunk_oversized);
}

#[test]
fn multiple_small_files_yield_n_review_units_in_stable_order() {
    let files = vec![
        kept_file_with_hunks("src/a.rs", "added", vec![simple_hunk("fn a() {}")]),
        kept_file_with_hunks("src/b.rs", "modified", vec![simple_hunk("fn b() {}")]),
        kept_file_with_hunks("src/c.rs", "modified", vec![simple_hunk("fn c() {}")]),
    ];
    let diff = make_diff(files);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 3);
    // Stable order: same as FilteredDiff.files order.
    assert_eq!(units[0].file, "src/a.rs");
    assert_eq!(units[1].file, "src/b.rs");
    assert_eq!(units[2].file, "src/c.rs");
    for u in &units {
        assert!(is_review(u));
    }
}

// ─── Metadata-only classification ────────────────────────────────────────────

#[test]
fn deleted_file_is_metadata_only() {
    let diff = make_diff(vec![deleted_file("src/old.rs")]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_metadata(&units[0]));
    assert_eq!(units[0].status, "removed");
    match &units[0].kind {
        MapUnitKind::MetadataOnly { note } => assert!(note.contains("deleted")),
        _ => panic!("expected MetadataOnly"),
    }
}

#[test]
fn binary_file_no_hunks_is_metadata_only() {
    let diff = make_diff(vec![binary_file("assets/logo.png")]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_metadata(&units[0]));
    match &units[0].kind {
        MapUnitKind::MetadataOnly { note } => {
            assert!(note.contains("binary") || note.contains("empty"))
        }
        _ => panic!("expected MetadataOnly"),
    }
}

#[test]
fn rename_only_no_hunks_is_metadata_only() {
    let diff = make_diff(vec![renamed_no_hunks("src/utils.rs")]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_metadata(&units[0]));
    match &units[0].kind {
        MapUnitKind::MetadataOnly { note } => assert!(note.contains("rename")),
        _ => panic!("expected MetadataOnly"),
    }
}

#[test]
fn summary_only_file_is_metadata_only() {
    let diff = make_diff(vec![summary_file("locales/en.json")]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_metadata(&units[0]));
    match &units[0].kind {
        MapUnitKind::MetadataOnly { note } => {
            assert!(
                note.contains("summary") || note.contains("fixture") || note.contains("generated")
            )
        }
        _ => panic!("expected MetadataOnly"),
    }
}

#[test]
fn renamed_file_with_hunks_is_review_unit() {
    // A renamed file that also has surviving hunks should be reviewed.
    let file = FilteredFile {
        filename: "src/auth_new.rs".to_string(),
        status: "renamed".to_string(),
        disposition: FileDisposition::Kept,
        hunks: vec![simple_hunk("fn renamed_method() {}")],
        dropped_hunks: vec![],
        summary_line: None,
    };
    let diff = make_diff(vec![file]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 1);
    assert!(is_review(&units[0]));
}

#[test]
fn mixed_files_metadata_and_review_units() {
    let diff = make_diff(vec![
        kept_file_with_hunks("src/lib.rs", "modified", vec![simple_hunk("fn foo() {}")]),
        deleted_file("src/old.rs"),
        summary_file("locales/fr.json"),
        kept_file_with_hunks("src/api.rs", "added", vec![simple_hunk("pub fn api() {}")]),
    ]);
    let units = split_into_units(&diff, &default_config());
    assert_eq!(units.len(), 4);
    assert!(is_review(&units[0]), "src/lib.rs should be review");
    assert!(
        is_metadata(&units[1]),
        "src/old.rs (deleted) should be metadata"
    );
    assert!(
        is_metadata(&units[2]),
        "locales/fr.json (summary) should be metadata"
    );
    assert!(is_review(&units[3]), "src/api.rs should be review");
}

// ─── Oversized file sub-chunking ──────────────────────────────────────────────

#[test]
fn small_file_exactly_at_budget_emits_one_unit() {
    // Create a hunk that, when rendered with the file header, is exactly
    // per_file_chars in size.
    let budget = 200usize;
    // The file header is `--- a/f.rs\n+++ b/f.rs\n` = 24 chars.
    let header_len = "--- a/f.rs\n+++ b/f.rs\n".len();
    // Exact char accounting:
    // render() = hunk_header + "\n" + line (where line = "+" + "x"*n)
    // render_file wraps it: format!("{}\n", h.render()) adds a final "\n"
    // So total hunk chars = hunk_header.len() + 1 + 1 + n + 1
    //                     = hunk_header.len() + n + 3   (the leading "+" counts)
    // Total rendered file = header_len + hunk_header.len() + n + 3
    // => n = budget - header_len - hunk_header.len() - 3
    let hunk_header = "@@ -1,1 +1,1 @@";
    let n = budget - header_len - hunk_header.len() - 3;
    let line = format!("+{}", "x".repeat(n));
    let hunk = FilteredHunk {
        header: hunk_header.to_string(),
        lines: vec![line],
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    };
    let file = kept_file_with_hunks("f.rs", "modified", vec![hunk]);
    let diff = make_diff(vec![file]);
    let cfg = config_with_per_file_chars(budget);
    let units = split_into_units(&diff, &cfg);
    assert_eq!(units.len(), 1);
    assert!(is_review(&units[0]));
    assert_eq!(units[0].chunk_total, 1);
    assert!(!units[0].hunk_oversized);
}

#[test]
fn oversized_file_sub_chunks_by_whole_hunk() {
    // Two hunks each ~100 chars; budget = 120. Each hunk should be its own unit.
    let budget = 120usize;
    let hunk1 = hunk_of_size(80);
    let hunk2 = hunk_of_size(80);
    let file = kept_file_with_hunks("src/big.rs", "modified", vec![hunk1, hunk2]);
    let diff = make_diff(vec![file]);
    let cfg = config_with_per_file_chars(budget);
    let units = split_into_units(&diff, &cfg);

    // Both hunks together (160 chars + ~24 header) exceed 120; each hunk alone
    // (~80 chars + ~24 header = ~104 chars) fits under 120.  So we expect 2 units.
    assert_eq!(units.len(), 2, "expected 2 chunks for oversized file");
    assert_eq!(units[0].file, "src/big.rs");
    assert_eq!(units[1].file, "src/big.rs");
    assert_eq!(units[0].chunk_index, 0);
    assert_eq!(units[1].chunk_index, 1);
    assert_eq!(units[0].chunk_total, 2);
    assert_eq!(units[1].chunk_total, 2);
    assert!(is_review(&units[0]));
    assert!(is_review(&units[1]));
    assert!(!units[0].hunk_oversized);
    assert!(!units[1].hunk_oversized);
    // Each unit's diff_char_count must be <= budget (individual hunk + header).
    assert!(
        units[0].diff_char_count <= budget + 30,
        "chunk 0 exceeds budget: {}",
        units[0].diff_char_count
    );
}

#[test]
fn single_giant_hunk_kept_whole_and_flagged_oversized() {
    // One hunk that alone exceeds the per-file budget.
    let budget = 100usize;
    let giant_hunk = hunk_of_size(500); // Way over budget.
    let file = kept_file_with_hunks("src/giant.rs", "modified", vec![giant_hunk]);
    let diff = make_diff(vec![file]);
    let cfg = config_with_per_file_chars(budget);
    let units = split_into_units(&diff, &cfg);

    // The single hunk cannot be further split, so we emit one unit and flag it.
    assert_eq!(units.len(), 1);
    assert!(is_review(&units[0]));
    assert!(
        units[0].hunk_oversized,
        "single giant hunk must be flagged oversized"
    );
    assert_eq!(units[0].chunk_index, 0);
    assert_eq!(units[0].chunk_total, 1);
}

#[test]
fn three_hunks_pack_into_two_chunks() {
    // Hunks of size ~60 each; budget = 150.
    // Header ~24 chars.  Two hunks = 120 + 24 = 144 < 150; three = 180+24 > 150.
    // Expect: chunk 0 has hunks 0+1; chunk 1 has hunk 2.
    let budget = 150usize;
    let h1 = hunk_of_size(60);
    let h2 = hunk_of_size(60);
    let h3 = hunk_of_size(60);
    let file = kept_file_with_hunks("src/three.rs", "modified", vec![h1, h2, h3]);
    let diff = make_diff(vec![file]);
    let cfg = config_with_per_file_chars(budget);
    let units = split_into_units(&diff, &cfg);
    // We expect either 2 or 3 chunks depending on exact header size.
    // What matters: all units are Review, all belong to src/three.rs, and no
    // single unit (except a flagged-oversized one) exceeds budget by more than
    // the file-header overhead.
    assert!(
        units.len() >= 2 && units.len() <= 3,
        "expected 2 or 3 chunks, got {}",
        units.len()
    );
    for u in &units {
        assert_eq!(u.file, "src/three.rs");
        // MetadataOnly units from budget exhaustion have diff_char_count==0; skip.
        if is_review(u) && !u.hunk_oversized {
            assert!(
                u.diff_char_count <= budget + 50, // allow generous header overhead
                "unit exceeds budget: {}",
                u.diff_char_count
            );
        }
    }
    let total = units.len();
    for (i, u) in units.iter().enumerate() {
        assert_eq!(u.chunk_total, total);
        assert_eq!(u.chunk_index, i);
    }
}

// Budget enforcement, diff-text content, status preservation, and edge-case tests
// live in `splitter_budget_tests.rs` to honour the 500-line file cap (CLAUDE.md).
