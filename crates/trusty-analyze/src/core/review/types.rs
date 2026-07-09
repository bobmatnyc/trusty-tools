//! Review report data types and the parsed-diff representation.
//!
//! Why: the wire/report structs are a self-contained concern lifted out of
//! `mod.rs` to keep it under the 500-SLOC cap (see #1195).
//! What: `ReviewError`, the per-file/report structs, and `FileDiff`.
//! Test: see `super`'s `tests` module.

use serde::{Deserialize, Serialize};

use crate::types::complexity::ComplexityGrade;

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
    pub(crate) fn touches_range(&self, start: usize, end: usize) -> bool {
        self.added_line_numbers
            .iter()
            .any(|&ln| (ln as usize) >= start && (ln as usize) <= end)
    }
}
