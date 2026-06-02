//! Unit tests for Stage B HunkFilter (spec REV-203–204).

use super::HunkFilter;
use crate::pipeline::diff_analyzer::{
    file_filter::FilterConfig,
    models::{FileDisposition, FilteredFile, FilteredHunk, HunkDropReason},
};

fn default_filter() -> HunkFilter {
    HunkFilter::new(&FilterConfig::default())
}

fn make_file(name: &str, hunk_lines: Vec<Vec<String>>) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::Kept,
        hunks: hunk_lines
            .into_iter()
            .enumerate()
            .map(|(i, lines)| FilteredHunk {
                header: format!("@@ -{i},1 +{i},1 @@"),
                lines,
                substantive_confidence: 1.0,
                reason_kept: "stage-a-pass".to_string(),
            })
            .collect(),
        dropped_hunks: vec![],
        summary_line: None,
    }
}

fn lines(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|s| s.to_string()).collect()
}

// ─── Whitespace-only ─────────────────────────────────────────────────────────

#[test]
fn whitespace_only_hunk_dropped() {
    let f = &mut make_file("src/a.rs", vec![lines(&["-   ", "+  "])]);
    let filter = default_filter();
    let counts = filter.filter_file(f);
    assert!(f.hunks.is_empty(), "whitespace hunk must be dropped");
    assert_eq!(f.dropped_hunks.len(), 1);
    assert_eq!(f.dropped_hunks[0].reason, HunkDropReason::WhitespaceOnly);
    assert_eq!(*counts.get(&HunkDropReason::WhitespaceOnly).unwrap(), 1);
}

#[test]
fn whitespace_gate_disabled_keeps_hunk() {
    let config = FilterConfig {
        ignore_whitespace: false,
        ..Default::default()
    };
    let filter = HunkFilter::new(&config);
    let f = &mut make_file("src/a.rs", vec![lines(&["-   ", "+  "])]);
    filter.filter_file(f);
    assert_eq!(
        f.hunks.len(),
        1,
        "whitespace hunk must be kept when gate disabled"
    );
    assert!(f.dropped_hunks.is_empty());
}

// ─── Import-only ──────────────────────────────────────────────────────────────

#[test]
fn rust_import_only_hunk_dropped() {
    let f = &mut make_file(
        "src/lib.rs",
        vec![lines(&["-use std::io;", "+use std::io::{Read, Write};"])],
    );
    let filter = default_filter();
    filter.filter_file(f);
    assert!(f.hunks.is_empty(), "import-only Rust hunk must be dropped");
    assert_eq!(f.dropped_hunks[0].reason, HunkDropReason::ImportOnly);
}

#[test]
fn python_import_only_hunk_dropped() {
    let f = &mut make_file(
        "service.py",
        vec![lines(&["-import os", "+import os, sys"])],
    );
    let filter = default_filter();
    filter.filter_file(f);
    assert!(
        f.hunks.is_empty(),
        "import-only Python hunk must be dropped"
    );
    assert_eq!(f.dropped_hunks[0].reason, HunkDropReason::ImportOnly);
}

#[test]
fn java_import_only_hunk_dropped() {
    let f = &mut make_file(
        "Foo.java",
        vec![lines(&[
            "-import java.util.List;",
            "+import java.util.List;",
            "+import java.util.Map;",
        ])],
    );
    let filter = default_filter();
    filter.filter_file(f);
    assert!(f.hunks.is_empty(), "import-only Java hunk must be dropped");
    assert_eq!(f.dropped_hunks[0].reason, HunkDropReason::ImportOnly);
}

#[test]
fn ts_import_only_hunk_dropped() {
    let f = &mut make_file(
        "index.ts",
        vec![lines(&[
            r#"-import { foo } from './foo';"#,
            r#"+import { foo, bar } from './foo';"#,
        ])],
    );
    let filter = default_filter();
    filter.filter_file(f);
    assert!(f.hunks.is_empty(), "import-only TS hunk must be dropped");
    assert_eq!(f.dropped_hunks[0].reason, HunkDropReason::ImportOnly);
}

#[test]
fn mixed_hunk_kept() {
    // Import line + logic line → must NOT be dropped (spec REV-203).
    let f = &mut make_file(
        "src/lib.rs",
        vec![lines(&["+use std::io;", "+fn process() {}"])],
    );
    let filter = default_filter();
    filter.filter_file(f);
    assert_eq!(f.hunks.len(), 1, "mixed hunk must be kept");
    assert!(f.dropped_hunks.is_empty());
}

// ─── Comment-only ─────────────────────────────────────────────────────────────

#[test]
fn comment_only_hunk_dropped() {
    let f = &mut make_file(
        "src/lib.rs",
        vec![lines(&["-// old comment", "+// new comment"])],
    );
    let filter = default_filter();
    filter.filter_file(f);
    assert!(f.hunks.is_empty(), "comment-only hunk must be dropped");
    assert_eq!(f.dropped_hunks[0].reason, HunkDropReason::CommentOnly);
}

// ─── Aggregate counts ─────────────────────────────────────────────────────────

#[test]
fn apply_returns_aggregate_counts() {
    let filter = default_filter();
    let mut files = vec![
        make_file("src/a.rs", vec![lines(&["-use foo;", "+use bar;"])]),
        make_file("src/b.rs", vec![lines(&["-  ", "+   "])]),
    ];
    let counts = filter.apply(&mut files);
    assert_eq!(*counts.get(&HunkDropReason::ImportOnly).unwrap_or(&0), 1);
    assert_eq!(
        *counts.get(&HunkDropReason::WhitespaceOnly).unwrap_or(&0),
        1
    );
}

// ─── Summary-only files skipped ───────────────────────────────────────────────

#[test]
fn summary_only_files_skipped_by_stage_b() {
    let filter = default_filter();
    let mut files = vec![FilteredFile {
        filename: "tests/fixtures/data.tsv".to_string(),
        status: "modified".to_string(),
        disposition: FileDisposition::SummaryOnly,
        hunks: vec![],
        dropped_hunks: vec![],
        summary_line: Some("[fixture: 50 lines omitted]".to_string()),
    }];
    let counts = filter.apply(&mut files);
    assert!(
        counts.is_empty(),
        "SummaryOnly files must be skipped by Stage B"
    );
}
