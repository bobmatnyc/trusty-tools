//! Unit tests for `FilteredDiff`, `FilteredHunk`, `FilteredFile`, `HunkDropReason`.
//!
//! Why: split from `models.rs` to keep that file under the 500-line cap (CLAUDE.md).
//! What: covers `render_for_prompt` (normal, budget-exceeded, mid-file overflow),
//! `build_noise_summary`, `FilteredHunk::render`, and `HunkDropReason::label`.
//! Test: see individual test functions.

use std::collections::HashMap;

use super::{
    DroppedFile, FileDisposition, FilteredDiff, FilteredFile, FilteredHunk, HunkDropReason,
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

pub(super) fn make_kept_file(name: &str, hunk_content: &str) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::Kept,
        hunks: vec![FilteredHunk {
            header: "@@ -1,3 +1,3 @@".to_string(),
            lines: vec![hunk_content.to_string()],
            substantive_confidence: 1.0,
            reason_kept: "deterministic-pass".to_string(),
        }],
        dropped_hunks: vec![],
        summary_line: None,
    }
}

// ─── FilteredHunk ─────────────────────────────────────────────────────────────

#[test]
fn filtered_hunk_render_roundtrip() {
    let h = FilteredHunk {
        header: "@@ -1,2 +1,2 @@".to_string(),
        lines: vec!["-old line".to_string(), "+new line".to_string()],
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    };
    let rendered = h.render();
    assert!(rendered.contains("@@ -1,2 +1,2 @@"));
    assert!(rendered.contains("-old line"));
    assert!(rendered.contains("+new line"));
}

// ─── HunkDropReason ───────────────────────────────────────────────────────────

#[test]
fn hunk_drop_reason_label() {
    assert_eq!(HunkDropReason::WhitespaceOnly.label(), "whitespace-only");
    assert_eq!(HunkDropReason::ImportOnly.label(), "import-only");
    assert_eq!(HunkDropReason::CommentOnly.label(), "comment-only");
    assert_eq!(
        HunkDropReason::MechanicalHaiku.label(),
        "mechanical (Haiku)"
    );
}

// ─── render_for_prompt — normal paths ─────────────────────────────────────────

#[test]
fn filtered_diff_render_for_prompt_contains_surviving_content() {
    let diff = FilteredDiff {
        files: vec![make_kept_file("src/auth.rs", "+pub fn authenticate() {}")],
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 500,
        filtered_byte_size: 100,
    };
    let rendered = diff.render_for_prompt(10_000);
    assert!(rendered.contains("src/auth.rs"), "file path must appear");
    assert!(
        rendered.contains("authenticate"),
        "hunk content must appear"
    );
}

#[test]
fn filtered_diff_render_respects_max_chars() {
    // Create a large number of files — rendering should stop before max_chars.
    let files: Vec<FilteredFile> = (0..100)
        .map(|i| make_kept_file(&format!("src/file{i}.rs"), &"+fn foo() {}".repeat(50)))
        .collect();
    let diff = FilteredDiff {
        files,
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 100_000,
        filtered_byte_size: 50_000,
    };
    let rendered = diff.render_for_prompt(2_000);
    // Allow for the truncation marker overhead (~300 chars) on top of max_chars.
    assert!(
        rendered.len() <= 2_000 + 400,
        "rendered output must not greatly exceed max_chars: len={}",
        rendered.len()
    );
}

// ─── build_noise_summary ──────────────────────────────────────────────────────

#[test]
fn filtered_diff_drop_summary_emitted() {
    let mut drop_counts = HashMap::new();
    drop_counts.insert(HunkDropReason::ImportOnly, 3u32);
    drop_counts.insert(HunkDropReason::WhitespaceOnly, 1u32);

    let diff = FilteredDiff {
        files: vec![make_kept_file("src/main.rs", "+fn main() {}")],
        dropped_files: vec![DroppedFile {
            path: "Cargo.lock".to_string(),
            reason: "lockfile".to_string(),
        }],
        drop_hunk_counts: drop_counts,
        original_byte_size: 5_000,
        filtered_byte_size: 200,
    };

    let rendered = diff.render_for_prompt(100_000);
    assert!(
        rendered.contains("DiffAnalyzer filtered"),
        "noise summary must appear: {rendered}"
    );
    assert!(
        rendered.contains("file(s) omitted"),
        "file drop count must appear: {rendered}"
    );
    assert!(
        rendered.contains("hunk(s) omitted"),
        "hunk drop count must appear: {rendered}"
    );
}

#[test]
fn no_summary_when_nothing_dropped() {
    let diff = FilteredDiff {
        files: vec![make_kept_file("src/lib.rs", "+pub fn new() {}")],
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 100,
        filtered_byte_size: 100,
    };
    let summary = diff.build_noise_summary();
    assert!(summary.is_empty(), "empty summary when nothing was dropped");
}

// ─── render_for_prompt — truncation / budget-exceeded paths ───────────────────

/// Regression: the inner hunk-loop `break` previously exited only the hunk
/// loop, allowing subsequent files to be appended after a half-rendered file
/// with no truncation marker.  This test verifies the fix: once the budget
/// is exhausted mid-file, no further files are rendered and a loud
/// `[RENDER TRUNCATED …]` marker is appended.
///
/// Why: silent mid-file truncation caused the reviewer LLM to see an
/// incomplete first file followed by complete later files, with no indication
/// that hunks were dropped.  The fix breaks the outer loop and announces the
/// truncation loudly (closes #622 / #624).
/// What: builds two files where the first file's second hunk overflows the
/// budget.  Asserts the second file does NOT appear and the truncation marker
/// DOES appear.
/// Test: this test itself.
#[test]
fn render_for_prompt_mid_file_hunk_overflow_loud_not_silent() {
    // File 1 has two hunks; the second one is large enough to overflow.
    // File 2 must NOT appear in the output.
    let large_hunk_content = "+".to_string() + &"x".repeat(900);
    let file1 = FilteredFile {
        filename: "src/first.rs".to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::Kept,
        hunks: vec![
            FilteredHunk {
                header: "@@ -1,1 +1,1 @@".to_string(),
                lines: vec!["+fn first() {}".to_string()],
                substantive_confidence: 1.0,
                reason_kept: "test".to_string(),
            },
            FilteredHunk {
                header: "@@ -10,1 +10,1 @@".to_string(),
                lines: vec![large_hunk_content],
                substantive_confidence: 1.0,
                reason_kept: "test".to_string(),
            },
        ],
        dropped_hunks: vec![],
        summary_line: None,
    };
    let file2 = make_kept_file("src/second.rs", "+fn second() {}");

    let diff = FilteredDiff {
        files: vec![file1, file2],
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 2_000,
        filtered_byte_size: 1_000,
    };

    // Budget: large enough for file1 header + first hunk, but NOT the second hunk.
    let rendered = diff.render_for_prompt(200);

    // The truncation marker must appear.
    assert!(
        rendered.contains("RENDER TRUNCATED"),
        "truncation marker must appear when budget is hit: {rendered}"
    );
    // The second file must NOT appear — the outer loop must have been broken.
    assert!(
        !rendered.contains("src/second.rs"),
        "second file must not appear after mid-file budget overflow: {rendered}"
    );
    // The rendered output must not greatly exceed the budget.
    assert!(
        rendered.len() <= 200 + 400, // budget + marker + suffix overhead
        "output must not greatly exceed max_chars: len={}",
        rendered.len()
    );
}

/// Verify that a diff that fits entirely within the budget has NO truncation
/// marker appended (no false-positive warnings).
///
/// Why: the truncation marker is a loud signal; it must not appear when all
/// content was rendered successfully.
/// What: renders a small diff well within a generous budget; asserts the
/// truncation marker is absent.
/// Test: this test itself.
#[test]
fn render_for_prompt_no_truncation_marker_when_fits() {
    let diff = FilteredDiff {
        files: vec![make_kept_file("src/lib.rs", "+pub fn new() {}")],
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 100,
        filtered_byte_size: 100,
    };
    let rendered = diff.render_for_prompt(100_000);
    assert!(
        !rendered.contains("RENDER TRUNCATED"),
        "no truncation marker when content fits: {rendered}"
    );
}

/// Verify that `render_for_prompt` respects the max_chars bound even after the
/// fix (the truncation marker + suffix must not push output far over the cap).
///
/// Why: the marker and suffix are added AFTER the main content loop; they must
/// not cause a large overflow.
/// What: builds a large diff exceeding the budget, calls render_for_prompt with
/// a tight cap, and asserts the result stays within a reasonable overhead band.
/// Test: this test itself.
#[test]
fn render_for_prompt_marker_does_not_cause_large_overflow() {
    let files: Vec<FilteredFile> = (0..50)
        .map(|i| make_kept_file(&format!("src/file{i}.rs"), &"+fn foo() {}".repeat(20)))
        .collect();
    let diff = FilteredDiff {
        files,
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 50_000,
        filtered_byte_size: 25_000,
    };
    let max_chars: usize = 500;
    let rendered = diff.render_for_prompt(max_chars);
    // Allow for the truncation marker (~200 chars) on top of max_chars.
    assert!(
        rendered.len() <= max_chars + 400,
        "rendered len {} must not greatly exceed max_chars {max_chars}",
        rendered.len()
    );
    assert!(
        rendered.contains("RENDER TRUNCATED"),
        "truncation marker must appear: {rendered}"
    );
}
