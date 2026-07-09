//! Unified-diff parser producing `FileDiff` slices.
//!
//! Why: the line-by-line diff scanner is a self-contained concern lifted out
//! of `mod.rs` to keep it under the 500-SLOC cap (see #1195).
//! What: `DiffParser::parse` plus the path/hunk-header helpers.
//! Test: see `super`'s `tests` module.

use super::types::{FileDiff, ReviewError};

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
