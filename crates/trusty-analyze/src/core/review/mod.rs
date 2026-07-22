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

use crate::core::client::TrustySearchClient;
use crate::core::complexity::compute_complexity_for;
use crate::types::complexity::{CodeSmell, ComplexityGrade};
use crate::types::CodeChunk;

mod parser;
#[cfg(test)]
mod tests;
mod types;

pub use parser::DiffParser;
pub use types::{
    FileDiff, FileReview, ReviewComplexity, ReviewError, ReviewReport, ReviewSource, SmellHit,
};

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
        // Reuse the smells already computed by `compute_complexity_for` above
        // instead of re-running the language-agnostic text heuristic (which
        // is documented to misfire on common idioms) — see #core/quality.rs
        // for the same fix on the whole-codebase quality report path.
        let smells = project_smells(&metrics.smells, anchor);
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
    let smells = project_smells(&metrics.smells, anchor);
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
