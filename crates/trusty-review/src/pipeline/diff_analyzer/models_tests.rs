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

// ─── #1660 follow-up: exact untruncated length vs the marker-based proxy ──
//
// `render_for_prompt`'s truncation-marker decision reserves headroom
// (`suffix.len() + 120`, `TRUNC_MARKER_RESERVE`) before it will place each
// segment, so it can set `RENDER TRUNCATED` for a diff whose TRUE, uncapped
// length is actually at or under `max_chars`.  A caller inferring "untruncated
// length > max_chars" from that marker's presence (as `runner.rs` briefly did
// on commit 73962068e) gets a false positive for any diff landing in that
// ~120-char reserve band just under the cap.  These two tests build such a
// diff and prove: (1) `total_rendered_len` returns its EXACT length, matching
// an unbounded render, even though the marker is a false positive here; (2)
// feeding that exact length into `select_review_mode` (what `runner.rs` now
// does) correctly picks `Unified`, while the OLD marker-based formula
// (reproduced inline, matching commit 73962068e) incorrectly picks
// `MapReduce`.

/// Build a one-file, one-hunk `FilteredDiff` whose real (uncapped) rendered
/// length is exactly `target` chars — no dropped files/hunks, so
/// `build_noise_summary` is empty and the only variables are the file header,
/// hunk header, and one padded content line.
fn diff_with_exact_rendered_len(target: usize) -> FilteredDiff {
    let filename = "src/a.rs";
    let file_header_len = format!("--- a/{filename}\n+++ b/{filename}\n").len();
    let hunk_header = "@@ -1,3 +1,3 @@";
    // total_rendered_len = file_header + (hunk_header.len() + line.len() + 1) + 1
    // (the `+1` inside is the hunk's own line-separator; the trailing `+1` is
    // the newline `render_for_prompt` pushes after each rendered hunk).
    let fixed_overhead = file_header_len + hunk_header.len() + 1 + 1;
    let line_len = target
        .checked_sub(fixed_overhead)
        .expect("target must exceed the fixed header/newline overhead");
    let line = format!("+{}", "x".repeat(line_len.saturating_sub(1)));

    FilteredDiff {
        files: vec![FilteredFile {
            filename: filename.to_string(),
            status: "modified".to_string(),
            disposition: FileDisposition::Kept,
            hunks: vec![FilteredHunk {
                header: hunk_header.to_string(),
                lines: vec![line],
                substantive_confidence: 1.0,
                reason_kept: "test".to_string(),
            }],
            dropped_hunks: vec![],
            summary_line: None,
        }],
        dropped_files: vec![],
        drop_hunk_counts: HashMap::new(),
        original_byte_size: target,
        filtered_byte_size: target,
    }
}

/// Test: this test.
#[test]
fn total_rendered_len_is_exact_inside_the_reserve_band_where_the_marker_lies() {
    let target = crate::config::constants::MAX_DIFF_CHARS - 60; // inside the 120-char reserve band, still <= cap
    let diff = diff_with_exact_rendered_len(target);

    assert_eq!(
        diff.total_rendered_len(),
        target,
        "total_rendered_len must be the exact constructed length"
    );
    assert_eq!(
        diff.total_rendered_len(),
        diff.render_for_prompt(usize::MAX).len(),
        "total_rendered_len must match what an unbounded render actually produces"
    );

    let bounded = diff.render_for_prompt(crate::config::constants::MAX_DIFF_CHARS);
    assert!(
        bounded.contains("RENDER TRUNCATED"),
        "the reserve-band diff must trip render_for_prompt's own conservative \
         marker even though its true length ({target}) is under the cap \
         ({}) — this is the false positive that made the marker unsafe as a \
         `diff_chars` proxy: {bounded}",
        crate::config::constants::MAX_DIFF_CHARS
    );
}

/// Why (#1660 follow-up): this reproduces the actual bug commit 73962068e
/// introduced. `runner.rs` derived `DiffStats::diff_chars` from
/// `render_for_prompt`'s marker (`max_chars + 1` whenever the marker was
/// set) — a false positive for this diff per the test above — which made
/// `select_review_mode` pick `MapReduce` for a diff that fits comfortably
/// under `MAX_DIFF_CHARS`. This proves both halves: the OLD (marker-based)
/// formula selects `MapReduce` here; the FIXED (`total_rendered_len`-based)
/// one selects `Unified`.
/// What: same reserve-band `FilteredDiff`, fed through `select_review_mode`
/// two ways.
/// Test: this test.
#[test]
fn reserve_band_diff_selects_unified_not_mapreduce() {
    use crate::config::{DiffStats, MapReduceConfig, ReviewPath, select_review_mode};
    use crate::pipeline::diff::diff_was_truncated;

    let max = crate::config::constants::MAX_DIFF_CHARS;
    let target = max - 60;
    let diff = diff_with_exact_rendered_len(target);
    let mr_config = MapReduceConfig::default(); // Auto mode, file_threshold=12 — 1 file never trips it

    // OLD formula (commit 73962068e): infer diff_chars from the served
    // render's truncation marker.
    let bounded = diff.render_for_prompt(max);
    let old_diff_chars = if diff_was_truncated(&bounded) {
        max.saturating_add(1)
    } else {
        bounded.len()
    };
    let old_stats = DiffStats {
        diff_chars: old_diff_chars,
        file_count: diff.files.len(),
    };
    assert_eq!(
        select_review_mode(old_stats, &mr_config),
        ReviewPath::MapReduce,
        "reproduces the bug on 73962068e: the marker-based diff_chars picks \
         MapReduce for a diff that is actually under the cap"
    );

    // Fixed formula: the exact untruncated length via total_rendered_len.
    let new_stats = DiffStats {
        diff_chars: diff.total_rendered_len(),
        file_count: diff.files.len(),
    };
    assert_eq!(
        select_review_mode(new_stats, &mr_config),
        ReviewPath::Unified,
        "the exact untruncated length correctly selects Unified for a diff \
         under the cap"
    );
}
