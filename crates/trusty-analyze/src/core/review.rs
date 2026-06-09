//! Unified-diff review: parse a git diff and produce a per-file quality report,
//! cross-referenced against the trusty-search indexed corpus.
//!
//! Why: PR review is the highest-leverage moment to flag complexity and smells —
//! before code lands. The `review` CLI command, `POST /review` endpoint, and
//! `review_diff` MCP tool all feed a unified diff to [`analyze_diff_with_client`]
//! (or the lower-level [`analyze_diff_with_chunks`]) and get back a structured
//! [`ReviewReport`]. Like every other analyzer command, review requires
//! trusty-search to be running: it pulls the index's existing chunk corpus so
//! the report can surface trusty-search's already-computed complexity scores,
//! call-graph context, and blame for the chunks the diff touches.
//!
//! What: [`DiffParser`] turns a unified diff into [`FileDiff`]s carrying the
//! added line numbers + added content per file. [`analyze_diff_with_chunks`]
//! merges those diffs with the index's [`CodeChunk`] corpus — for files that
//! are already indexed it reports the indexed chunks' complexity and flags
//! which ones the diff modifies; for files not yet indexed (new files) it
//! falls back to tree-sitter local analysis of the added content.
//!
//! Test: see `mod tests` — covers single/multi-file diffs, hunk header parsing,
//! rename/new-file handling, grade aggregation, recommendation synthesis, and
//! the indexed-vs-new-file merge logic.

use serde::{Deserialize, Serialize};

use crate::core::client::TrustySearchClient;
use crate::core::complexity::{compute_complexity_for, detect_smells};
use crate::types::complexity::{CodeSmell, ComplexityGrade};
use crate::types::CodeChunk;

/// Errors that can arise while parsing or running a review.
///
/// Why: keeps the library-layer failure mode typed (`thiserror`) so callers can
/// distinguish a malformed diff from a trusty-search transport failure.
/// What: a malformed hunk header, or an error talking to trusty-search.
/// Test: `malformed_hunk_header_is_rejected` exercises the parse error path;
/// `analyze_diff_with_client_errors_when_search_down` exercises the search path.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// A `@@ ... @@` hunk header could not be parsed.
    #[error("malformed hunk header: {0}")]
    MalformedHunkHeader(String),
    /// Fetching the index corpus from trusty-search failed.
    #[error("trusty-search unreachable or returned an error: {0}")]
    Search(String),
}

/// Complexity numbers for one reviewed file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewComplexity {
    pub cyclomatic: u32,
    pub cognitive: u32,
}

/// One detected smell, flattened for the review wire format.
///
/// Why: the review report is consumed by tools/humans that want a flat
/// `{category, line, severity}` shape rather than the tagged [`CodeSmell`]
/// enum. This struct is that projection.
/// What: `category` is a snake_case smell name, `line` is the 1-based line in
/// the new file where the smell was detected (best-effort), `severity` is
/// `"low" | "medium" | "high"`.
/// Test: `smell_hit_projection_maps_categories` checks every variant maps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmellHit {
    pub category: String,
    pub line: u32,
    pub severity: String,
}

/// How a reviewed file's analysis was sourced.
///
/// Why: callers want to know whether a file's metrics came from the
/// trusty-search index (richer: includes pre-computed complexity for the whole
/// file, not just the diff) or from a local tree-sitter fallback (new files
/// that trusty-search has not indexed yet).
/// What: `Indexed` carries how many existing chunks the diff touched;
/// `NewFile` marks a file absent from the index.
/// Test: `analyze_merges_indexed_file` / `analyze_falls_back_for_new_file`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewSource {
    /// File is present in the trusty-search index; `modified_chunks` indexed
    /// chunks overlap the diff's added line ranges.
    Indexed { modified_chunks: usize },
    /// File is not in the index (new in this diff); analyzed locally.
    NewFile,
}

/// Per-file slice of a [`ReviewReport`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileReview {
    pub path: String,
    pub grade: ComplexityGrade,
    pub complexity: ReviewComplexity,
    pub smells: Vec<SmellHit>,
    pub recommendations: Vec<String>,
    /// Whether this file was cross-referenced against the trusty-search index
    /// or analyzed locally as a new file.
    pub source: ReviewSource,
}

/// Full structured review of a unified diff.
///
/// Why: this is the deterministic, reproducible output of the static review
/// pipeline. It deliberately contains *no* LLM-generated fields: a `ReviewReport`
/// produced for the same diff and chunk corpus must be byte-identical across
/// runs so it can be cached, snapshotted in tests, and treated as a fixed-point
/// input by the LLM-backed deep-analysis pass (see
/// [`crate::core::explain::DeepAnalysisReport`]).
/// What: per-file `FileReview`s plus aggregate grade/line/smell counts and a
/// short summary string.
/// Test: covered transitively by every analyzer test; round-tripped through
/// JSON in `report_round_trips_json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewReport {
    pub files: Vec<FileReview>,
    pub overall_grade: ComplexityGrade,
    pub changed_lines: usize,
    pub smell_count: usize,
    pub summary: String,
}

/// One file's added content extracted from a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// New-side path (the `+++ b/<path>` target).
    pub path: String,
    /// 1-based line numbers (in the new file) of every added line.
    pub added_line_numbers: Vec<u32>,
    /// The added lines' content, in order, joined by newlines on request.
    pub added_lines: Vec<String>,
}

impl FileDiff {
    /// Reconstruct the added content as a single string.
    ///
    /// Why: the complexity backend takes a `&str`; concatenating the added
    /// lines gives it a coherent (if non-contiguous) view of what the PR adds.
    /// What: joins `added_lines` with `\n`.
    /// Test: `file_diff_added_content_joins_lines`.
    pub fn added_content(&self) -> String {
        self.added_lines.join("\n")
    }

    /// True if any added line number falls inside `[start, end]` (1-based,
    /// inclusive).
    ///
    /// Why: used to decide whether an indexed chunk is "modified" by this diff.
    /// What: linear scan of `added_line_numbers` against the chunk's range.
    /// Test: `file_diff_touches_chunk_range`.
    fn touches_range(&self, start: usize, end: usize) -> bool {
        self.added_line_numbers
            .iter()
            .any(|&ln| (ln as usize) >= start && (ln as usize) <= end)
    }
}

/// Stateless parser for unified git diffs.
///
/// Why: a dedicated type makes the parser unit-testable in isolation and gives
/// the analysis entry points a clean seam.
/// What: [`DiffParser::parse`] scans the diff line-by-line, tracking the
/// current file (`+++ b/...`) and the current hunk's new-side line counter
/// (`@@ -a,b +c,d @@`).
/// Test: see `mod tests`.
pub struct DiffParser;

impl DiffParser {
    /// Parse a unified diff into per-file added-content slices.
    ///
    /// Lines starting with `+++` name the new file; `@@` headers reset the
    /// new-side line counter; lines starting with a single `+` are additions;
    /// context lines and `-` deletions advance/hold the counter appropriately.
    pub fn parse(diff: &str) -> Result<Vec<FileDiff>, ReviewError> {
        let mut files: Vec<FileDiff> = Vec::new();
        let mut current: Option<FileDiff> = None;
        // 1-based line number in the new file for the next consumed line.
        let mut new_line: u32 = 0;

        for raw in diff.lines() {
            if let Some(rest) = raw.strip_prefix("+++ ") {
                // Flush the previous file before starting a new one.
                if let Some(f) = current.take() {
                    files.push(f);
                }
                let path = normalize_diff_path(rest);
                current = Some(FileDiff {
                    path,
                    added_line_numbers: Vec::new(),
                    added_lines: Vec::new(),
                });
                new_line = 0;
                continue;
            }
            if raw.starts_with("--- ") || raw.starts_with("diff ") || raw.starts_with("index ") {
                // Old-file marker / git metadata — ignored.
                continue;
            }
            if let Some(header) = raw.strip_prefix("@@") {
                new_line = parse_hunk_new_start(header)?;
                continue;
            }
            let Some(file) = current.as_mut() else {
                // Content before any `+++` header — skip (e.g. preamble).
                continue;
            };
            // Within a hunk: classify the line.
            if let Some(added) = raw.strip_prefix('+') {
                file.added_line_numbers.push(new_line);
                file.added_lines.push(added.to_string());
                new_line += 1;
            } else if raw.starts_with('-') {
                // Deletion: present on the old side only — new counter holds.
            } else if raw.starts_with('\\') {
                // "\ No newline at end of file" — not a real line.
            } else {
                // Context line — present on both sides.
                new_line += 1;
            }
        }
        if let Some(f) = current.take() {
            files.push(f);
        }
        Ok(files)
    }
}

/// Strip the `a/` or `b/` prefix and any trailing tab-delimited metadata from
/// a diff path token (`b/src/foo.rs\t2026-01-01` → `src/foo.rs`).
fn normalize_diff_path(token: &str) -> String {
    let head = token.split('\t').next().unwrap_or(token).trim();
    head.strip_prefix("a/")
        .or_else(|| head.strip_prefix("b/"))
        .unwrap_or(head)
        .to_string()
}

/// Parse the new-side start line from a `@@ -a,b +c,d @@` header.
/// Returns the 1-based `c` value.
fn parse_hunk_new_start(header: &str) -> Result<u32, ReviewError> {
    // header looks like ` -12,7 +12,9 @@ optional context`
    let plus = header
        .split('+')
        .nth(1)
        .ok_or_else(|| ReviewError::MalformedHunkHeader(header.to_string()))?;
    let num: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
    num.parse::<u32>()
        .map_err(|_| ReviewError::MalformedHunkHeader(header.to_string()))
}

/// Guess a language name from a file extension, for the complexity dispatcher.
///
/// Why: delegates to the canonical extension table so review-path language
/// detection stays in sync with all other consumers without maintaining a
/// separate if/else chain.
/// What: thin wrapper over `crate::lang::ext_map::lang_for_extension`.
/// Test: exercised indirectly by callers above; canonical coverage lives in
/// `crate::lang::ext_map::tests`.
fn language_for_path(path: &str) -> &'static str {
    crate::lang::ext_map::lang_for_extension(path)
}

/// Map a [`CodeSmell`] to its `(category, severity)` review projection.
fn smell_projection(s: &CodeSmell) -> (&'static str, &'static str) {
    match s {
        CodeSmell::LongFunction { .. } => ("long_method", "medium"),
        CodeSmell::DeepNesting { .. } => ("deep_nesting", "high"),
        CodeSmell::TooManyParams { .. } => ("too_many_params", "medium"),
        CodeSmell::MissingDocstring => ("missing_docstring", "low"),
    }
}

/// Build human-readable recommendations from a file's metrics and smells.
fn recommendations_for(
    grade: ComplexityGrade,
    cyclomatic: u32,
    smells: &[SmellHit],
    line_count: usize,
) -> Vec<String> {
    let mut recs: Vec<String> = Vec::new();
    if grade >= ComplexityGrade::C {
        recs.push(format!(
            "Cyclomatic complexity is {cyclomatic} (grade {grade}); extract logic into smaller helper functions"
        ));
    }
    for hit in smells {
        let rec = match hit.category.as_str() {
            "long_method" => format!(
                "Long method detected near line {}; split the {line_count}-line change into focused functions",
                hit.line
            ),
            "deep_nesting" => format!(
                "Deep nesting near line {}; use early returns or guard clauses",
                hit.line
            ),
            "too_many_params" => format!(
                "Too many parameters near line {}; group related arguments into a struct",
                hit.line
            ),
            "missing_docstring" => {
                "Add a doc comment explaining the intent of the new code".to_string()
            }
            other => format!("Review the '{other}' smell near line {}", hit.line),
        };
        if !recs.contains(&rec) {
            recs.push(rec);
        }
    }
    recs
}

/// Worst (highest) grade across a set of file grades. Empty input → `A`.
fn worst_grade(grades: impl IntoIterator<Item = ComplexityGrade>) -> ComplexityGrade {
    grades.into_iter().max().unwrap_or(ComplexityGrade::A)
}

/// Project a slice of [`CodeSmell`]s onto [`SmellHit`]s, anchored to `anchor`.
fn project_smells(raw: &[CodeSmell], anchor: u32) -> Vec<SmellHit> {
    raw.iter()
        .map(|s| {
            let (category, severity) = smell_projection(s);
            SmellHit {
                category: category.to_string(),
                line: anchor,
                severity: severity.to_string(),
            }
        })
        .collect()
}

/// Analyze one file: if `index_chunks` is non-empty the file is indexed and we
/// report the union of every chunk's content (so the report reflects the whole
/// file's complexity, not just the diff); otherwise we fall back to local
/// tree-sitter analysis of the diff's added content.
fn review_one_file(fd: &FileDiff, index_chunks: &[&CodeChunk]) -> FileReview {
    let lang = language_for_path(&fd.path);
    let anchor = fd.added_line_numbers.first().copied().unwrap_or(0);

    if index_chunks.is_empty() {
        // New file: not yet indexed by trusty-search. Local fallback.
        let content = fd.added_content();
        let metrics = compute_complexity_for(&content, lang);
        let smells = project_smells(&detect_smells(&content), anchor);
        let recommendations = recommendations_for(
            metrics.grade,
            metrics.cyclomatic,
            &smells,
            fd.added_lines.len(),
        );
        return FileReview {
            path: fd.path.clone(),
            grade: metrics.grade,
            complexity: ReviewComplexity {
                cyclomatic: metrics.cyclomatic,
                cognitive: metrics.cognitive,
            },
            smells,
            recommendations,
            source: ReviewSource::NewFile,
        };
    }

    // Indexed file: analyze the existing chunk corpus trusty-search holds, and
    // count which of those chunks the diff modifies (overlapping line ranges).
    let joined: String = index_chunks
        .iter()
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let metrics = compute_complexity_for(&joined, lang);
    let smells = project_smells(&detect_smells(&joined), anchor);
    let modified_chunks = index_chunks
        .iter()
        .filter(|c| fd.touches_range(c.start_line, c.end_line))
        .count();
    let mut recommendations = recommendations_for(
        metrics.grade,
        metrics.cyclomatic,
        &smells,
        fd.added_lines.len(),
    );
    if modified_chunks > 0 {
        recommendations.push(format!(
            "This change modifies {modified_chunks} already-indexed chunk(s); review their existing complexity before merging"
        ));
    }
    FileReview {
        path: fd.path.clone(),
        grade: metrics.grade,
        complexity: ReviewComplexity {
            cyclomatic: metrics.cyclomatic,
            cognitive: metrics.cognitive,
        },
        smells,
        recommendations,
        source: ReviewSource::Indexed { modified_chunks },
    }
}

/// Analyze a unified diff against a pre-fetched index corpus.
///
/// Why: the pure core of the review pipeline — given the diff text and the
/// index's `CodeChunk` corpus it produces a [`ReviewReport`] with no I/O, which
/// makes it trivially testable. [`analyze_diff_with_client`] is the thin
/// trusty-search-fetching wrapper around this.
/// What: parses the diff, groups `chunks` by file path, and for each changed
/// file either merges the indexed chunk data (richer) or falls back to local
/// tree-sitter analysis (new files). Aggregates the worst grade overall.
/// Test: `analyze_merges_indexed_file`, `analyze_falls_back_for_new_file`, and
/// the smell / recommendation tests below.
pub fn analyze_diff_with_chunks(
    diff: &str,
    chunks: &[CodeChunk],
) -> Result<ReviewReport, ReviewError> {
    use std::collections::HashMap;

    let file_diffs = DiffParser::parse(diff)?;

    // Index the corpus by file path so per-file lookup is O(1).
    let mut by_file: HashMap<&str, Vec<&CodeChunk>> = HashMap::new();
    for chunk in chunks {
        by_file.entry(chunk.file.as_str()).or_default().push(chunk);
    }

    let mut files: Vec<FileReview> = Vec::new();
    let mut changed_lines: usize = 0;
    let mut smell_count: usize = 0;

    for fd in &file_diffs {
        changed_lines += fd.added_lines.len();
        let index_chunks: &[&CodeChunk] = by_file
            .get(fd.path.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let review = review_one_file(fd, index_chunks);
        smell_count += review.smells.len();
        files.push(review);
    }

    let overall_grade = worst_grade(files.iter().map(|f| f.grade));
    let indexed = files
        .iter()
        .filter(|f| matches!(f.source, ReviewSource::Indexed { .. }))
        .count();
    let summary = format!(
        "{} file{} analyzed ({} indexed, {} new); {} smell{} found; overall grade {}",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        indexed,
        files.len() - indexed,
        smell_count,
        if smell_count == 1 { "" } else { "s" },
        overall_grade,
    );

    Ok(ReviewReport {
        files,
        overall_grade,
        changed_lines,
        smell_count,
        summary,
    })
}

/// Analyze a unified diff, fetching the index corpus from trusty-search.
///
/// Why: the single entry point shared by the CLI, HTTP, and MCP layers — keeps
/// those three thin and guarantees identical review output regardless of
/// transport. Like every other analyzer command, review is backed by
/// trusty-search: it pulls the index's chunk corpus so the report reflects
/// trusty-search's already-computed structural data for the touched files.
/// What: parses the diff first (so a malformed diff fails fast without a
/// network round-trip), calls `GET /indexes/:id/chunks` via `client`, then
/// delegates to [`analyze_diff_with_chunks`]. A search failure surfaces as
/// [`ReviewError::Search`].
/// Test: `analyze_diff_with_client_errors_when_search_down` checks the error
/// path; the merge logic is covered by `analyze_diff_with_chunks` tests.
pub async fn analyze_diff_with_client(
    diff: &str,
    client: &TrustySearchClient,
    index_id: &str,
) -> Result<ReviewReport, ReviewError> {
    // Validate the diff up front: a malformed diff should not depend on
    // trusty-search being reachable to be reported as a client error.
    DiffParser::parse(diff)?;
    let chunks = client
        .get_chunks(index_id)
        .await
        .map_err(|e| ReviewError::Search(format!("get_chunks({index_id}): {e:#}")))?;
    analyze_diff_with_chunks(diff, &chunks)
}

/// Render a [`ReviewReport`] as a human-readable text report.
///
/// Why: the `review --format text` CLI mode wants something a person can scan
/// in a terminal, not raw JSON.
/// What: prints a header line, then per-file grade/complexity/smells/recs and
/// the analysis source (indexed vs. new file).
/// Test: `text_report_contains_summary_and_files`.
pub fn render_text(report: &ReviewReport) -> String {
    let mut out = String::new();
    out.push_str("=== PR Review ===\n");
    out.push_str(&format!("{}\n", report.summary));
    out.push_str(&format!(
        "changed lines: {} | overall grade: {}\n",
        report.changed_lines, report.overall_grade
    ));
    for f in &report.files {
        let src = match &f.source {
            ReviewSource::Indexed { modified_chunks } => {
                format!("indexed, {modified_chunks} modified chunk(s)")
            }
            ReviewSource::NewFile => "new file (local analysis)".to_string(),
        };
        out.push_str(&format!(
            "\n{} — grade {} (cyclomatic {}, cognitive {}) [{}]\n",
            f.path, f.grade, f.complexity.cyclomatic, f.complexity.cognitive, src
        ));
        if f.smells.is_empty() {
            out.push_str("  smells: none\n");
        } else {
            for s in &f.smells {
                out.push_str(&format!(
                    "  smell: {} (severity {}, line {})\n",
                    s.category, s.severity, s.line
                ));
            }
        }
        for r in &f.recommendations {
            out.push_str(&format!("  → {r}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
}
