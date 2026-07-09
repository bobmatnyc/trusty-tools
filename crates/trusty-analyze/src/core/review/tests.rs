//! Unit tests for the diff-review pipeline.
//!
//! Why: lifted out of `mod.rs` to keep the production module under the
//! 500-SLOC cap; behaviour is unchanged (see #1195).

use super::*;

fn chunk(file: &str, start: usize, end: usize, content: &str) -> CodeChunk {
    CodeChunk {
        id: format!("{file}:{start}:{end}"),
        file: file.to_string(),
        start_line: start,
        end_line: end,
        content: content.to_string(),
        ..Default::default()
    }
}

#[test]
fn parses_single_file_addition() {
    let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -1,2 +1,4 @@
 fn existing() {}
+fn added() {
+    let x = 1;
+}
";
    let files = DiffParser::parse(diff).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "src/foo.rs");
    assert_eq!(files[0].added_lines.len(), 3);
    // Context line is line 1; the three additions are lines 2,3,4.
    assert_eq!(files[0].added_line_numbers, vec![2, 3, 4]);
}

#[test]
fn parses_multi_file_diff() {
    let diff = "\
+++ b/a.rs
@@ -0,0 +1,1 @@
+fn a() {}
+++ b/b.py
@@ -0,0 +1,1 @@
+def b(): pass
";
    let files = DiffParser::parse(diff).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "a.rs");
    assert_eq!(files[1].path, "b.py");
}

#[test]
fn deletion_lines_do_not_advance_new_counter() {
    let diff = "\
+++ b/x.rs
@@ -1,3 +1,2 @@
 fn keep() {}
-fn removed() {}
+fn replacement() {}
";
    let files = DiffParser::parse(diff).unwrap();
    // Context line 1, deletion ignored, addition lands on new line 2.
    assert_eq!(files[0].added_line_numbers, vec![2]);
}

#[test]
fn malformed_hunk_header_is_rejected() {
    let diff = "+++ b/x.rs\n@@ totally bogus @@\n+fn x() {}\n";
    let err = analyze_diff_with_chunks(diff, &[]).unwrap_err();
    assert!(matches!(err, ReviewError::MalformedHunkHeader(_)));
}

#[test]
fn file_diff_added_content_joins_lines() {
    let fd = FileDiff {
        path: "f.rs".into(),
        added_line_numbers: vec![1, 2],
        added_lines: vec!["fn f() {".into(), "}".into()],
    };
    assert_eq!(fd.added_content(), "fn f() {\n}");
}

#[test]
fn file_diff_touches_chunk_range() {
    let fd = FileDiff {
        path: "f.rs".into(),
        added_line_numbers: vec![5, 6, 7],
        added_lines: vec!["a".into(), "b".into(), "c".into()],
    };
    assert!(fd.touches_range(1, 6));
    assert!(fd.touches_range(7, 20));
    assert!(!fd.touches_range(8, 12));
}

#[test]
fn smell_hit_projection_maps_categories() {
    assert_eq!(
        smell_projection(&CodeSmell::LongFunction { lines: 99 }).0,
        "long_method"
    );
    assert_eq!(
        smell_projection(&CodeSmell::DeepNesting { max_depth: 7 }).0,
        "deep_nesting"
    );
    assert_eq!(
        smell_projection(&CodeSmell::TooManyParams { count: 9 }).0,
        "too_many_params"
    );
    assert_eq!(
        smell_projection(&CodeSmell::MissingDocstring).0,
        "missing_docstring"
    );
}

#[test]
fn analyze_falls_back_for_new_file() {
    // No chunk for src/foo.rs → treated as a new file, analyzed locally.
    let diff = "\
+++ b/src/foo.rs
@@ -0,0 +1,3 @@
+/// doc
+fn added() {}
";
    let report = analyze_diff_with_chunks(diff, &[]).unwrap();
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, "src/foo.rs");
    assert_eq!(report.files[0].source, ReviewSource::NewFile);
    assert_eq!(report.files[0].grade, ComplexityGrade::A);
    assert_eq!(report.overall_grade, ComplexityGrade::A);
    assert_eq!(report.changed_lines, 2);
    assert!(report.summary.contains("1 new"));
}

#[test]
fn analyze_merges_indexed_file() {
    // src/foo.rs IS indexed: two chunks, one of which the diff modifies.
    let chunks = vec![
        chunk("src/foo.rs", 1, 5, "fn existing() { let x = 1; }"),
        chunk("src/foo.rs", 10, 20, "fn other() {}"),
    ];
    // Diff adds lines at new-line 3 → overlaps the [1,5] chunk.
    let diff = "\
+++ b/src/foo.rs
@@ -1,4 +1,5 @@
 fn existing() {
 let x = 1;
+let y = 2;
 }
";
    let report = analyze_diff_with_chunks(diff, &chunks).unwrap();
    assert_eq!(report.files.len(), 1);
    match report.files[0].source {
        ReviewSource::Indexed { modified_chunks } => assert_eq!(modified_chunks, 1),
        ReviewSource::NewFile => panic!("expected indexed source"),
    }
    assert!(report.files[0]
        .recommendations
        .iter()
        .any(|r| r.contains("already-indexed chunk")));
    assert!(report.summary.contains("1 indexed"));
}

#[test]
fn analyze_mixed_indexed_and_new_files() {
    let chunks = vec![chunk("indexed.rs", 1, 3, "fn a() {}")];
    let diff = "\
+++ b/indexed.rs
@@ -1,1 +1,2 @@
 fn a() {}
+fn a2() {}
+++ b/brand_new.rs
@@ -0,0 +1,1 @@
+fn b() {}
";
    let report = analyze_diff_with_chunks(diff, &chunks).unwrap();
    assert_eq!(report.files.len(), 2);
    assert!(matches!(
        report.files[0].source,
        ReviewSource::Indexed { .. }
    ));
    assert_eq!(report.files[1].source, ReviewSource::NewFile);
    assert!(report.summary.contains("1 indexed, 1 new"));
}

#[test]
fn analyze_detects_long_method_smell_in_new_file() {
    let mut diff = String::from("+++ b/big.rs\n@@ -0,0 +1,60 @@\n");
    for _ in 0..60 {
        diff.push_str("+    let _ = 1;\n");
    }
    let report = analyze_diff_with_chunks(&diff, &[]).unwrap();
    assert!(report.smell_count >= 1);
    assert!(report.files[0]
        .smells
        .iter()
        .any(|s| s.category == "long_method"));
}

#[test]
fn analyze_empty_diff_is_grade_a() {
    let report = analyze_diff_with_chunks("", &[]).unwrap();
    assert!(report.files.is_empty());
    assert_eq!(report.overall_grade, ComplexityGrade::A);
    assert_eq!(report.changed_lines, 0);
    assert_eq!(report.smell_count, 0);
}

#[test]
fn text_report_contains_summary_and_files() {
    let diff = "+++ b/foo.rs\n@@ -0,0 +1,2 @@\n+/// doc\n+fn f() {}\n";
    let report = analyze_diff_with_chunks(diff, &[]).unwrap();
    let text = render_text(&report);
    assert!(text.contains("=== PR Review ==="));
    assert!(text.contains("foo.rs"));
    assert!(text.contains("overall grade"));
    assert!(text.contains("new file"));
}

#[test]
fn report_round_trips_json() {
    let diff = "+++ b/foo.rs\n@@ -0,0 +1,2 @@\n+/// doc\n+fn f() {}\n";
    let report = analyze_diff_with_chunks(diff, &[]).unwrap();
    let json = serde_json::to_string(&report).unwrap();
    let back: ReviewReport = serde_json::from_str(&json).unwrap();
    assert_eq!(report, back);
}

#[tokio::test]
async fn analyze_diff_with_client_errors_when_search_down() {
    // Client points at a dead port; the search fetch must fail with
    // ReviewError::Search rather than panicking.
    let client = TrustySearchClient::new("http://127.0.0.1:1");
    let diff = "+++ b/foo.rs\n@@ -0,0 +1,1 @@\n+fn f() {}\n";
    let err = analyze_diff_with_client(diff, &client, "idx")
        .await
        .expect_err("search down should error");
    assert!(matches!(err, ReviewError::Search(_)));
}
