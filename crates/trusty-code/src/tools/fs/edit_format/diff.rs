//! Minimal unified-diff hunk parser + applier for the `unified_diff` edit format.
//!
//! Why: #2068's fallback format between SEARCH/REPLACE and whole-file
//! replacement is a unified diff — the format models most often produce when
//! prompted for a "diff" (it is ubiquitous in code-training corpora). No
//! diff-apply crate is already a workspace dependency (see `Cargo.toml`); a
//! hand-rolled applier is small enough to keep in-tree rather than adding one
//! for a single call site.
//! What: [`apply_unified_diff`] parses one or more `@@ -l,s +l,s @@` hunks
//! (skipping optional `---`/`+++`/`diff --git`/`index` header lines) and
//! applies them against the file's current content, verifying every context
//! and removal line matches before committing the hunk.
//! Test: `tests::*` cover single-hunk, multi-hunk, mismatched-context, and
//! unparsable-header cases.

use std::path::Path;

use crate::tools::fs::FsError;

/// Apply a unified diff (`diff_text`) against `content`, returning the patched
/// content.
///
/// Why: Central entry point used by `edit_format::apply_payload`.
/// What: Parses hunks from `diff_text`, applies each in order against
/// `content`'s lines, and returns the full patched text (preserving whether
/// the original had a trailing newline). Returns `FsError::DiffHunkHeader` if
/// no valid hunk header is found, or `FsError::DiffContextMismatch` if a
/// context/removal line does not match the file at the expected position.
/// Test: `tests::apply_single_hunk`, `tests::apply_multiple_hunks`,
/// `tests::apply_context_mismatch_errors`, `tests::apply_no_hunks_errors`.
pub(crate) fn apply_unified_diff(
    content: &str,
    diff_text: &str,
    path: &Path,
) -> Result<String, FsError> {
    let hunks = parse_hunks(diff_text)?;
    if hunks.is_empty() {
        return Err(FsError::DiffHunkHeader {
            reason: "no '@@ -l,s +l,s @@' hunk header found in diff".to_string(),
        });
    }

    let had_trailing_newline = content.ends_with('\n');
    let orig_lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(orig_lines.len());
    let mut cursor = 0usize; // 0-based index into orig_lines already emitted/consumed

    for hunk in &hunks {
        let start = hunk.old_start.saturating_sub(1); // 0-based
        if start < cursor || start > orig_lines.len() {
            return Err(FsError::DiffContextMismatch {
                path: path.to_path_buf(),
                line: hunk.old_start,
                reason: "hunk start is out of order or past end of file".to_string(),
            });
        }
        // Copy unchanged lines up to the hunk's start.
        out.extend(orig_lines[cursor..start].iter().map(|s| s.to_string()));
        cursor = start;

        for line in &hunk.lines {
            match line {
                HunkLine::Context(text) => {
                    verify_line(orig_lines.get(cursor), text, hunk.old_start, path)?;
                    out.push(text.clone());
                    cursor += 1;
                }
                HunkLine::Removal(text) => {
                    verify_line(orig_lines.get(cursor), text, hunk.old_start, path)?;
                    cursor += 1;
                }
                HunkLine::Addition(text) => {
                    out.push(text.clone());
                }
            }
        }
    }
    out.extend(orig_lines[cursor..].iter().map(|s| s.to_string()));

    let mut result = out.join("\n");
    if had_trailing_newline {
        result.push('\n');
    }
    Ok(result)
}

/// Verify that `actual` (the file's current line at `cursor`) equals `expected`.
fn verify_line(
    actual: Option<&&str>,
    expected: &str,
    hunk_line: usize,
    path: &Path,
) -> Result<(), FsError> {
    match actual {
        Some(a) if *a == expected => Ok(()),
        Some(a) => Err(FsError::DiffContextMismatch {
            path: path.to_path_buf(),
            line: hunk_line,
            reason: format!("expected {expected:?}, found {a:?}"),
        }),
        None => Err(FsError::DiffContextMismatch {
            path: path.to_path_buf(),
            line: hunk_line,
            reason: format!("expected {expected:?}, found end of file"),
        }),
    }
}

/// One line inside a hunk body, tagged by its unified-diff prefix character.
#[derive(Debug, Clone, PartialEq)]
enum HunkLine {
    Context(String),
    Removal(String),
    Addition(String),
}

/// A single parsed `@@ -old_start,old_len +new_start,new_len @@` hunk.
#[derive(Debug, Clone, PartialEq)]
struct Hunk {
    old_start: usize,
    lines: Vec<HunkLine>,
}

/// Parse every hunk out of a unified-diff text, skipping file-header lines.
fn parse_hunks(diff_text: &str) -> Result<Vec<Hunk>, FsError> {
    let mut hunks = Vec::new();
    let mut current: Option<Hunk> = None;

    for raw_line in diff_text.lines() {
        if let Some(header) = raw_line.strip_prefix("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            let old_start = parse_hunk_header(header)?;
            current = Some(Hunk {
                old_start,
                lines: Vec::new(),
            });
            continue;
        }

        // Skip standard file-header noise that may precede the first hunk.
        if current.is_none()
            && (raw_line.starts_with("--- ")
                || raw_line.starts_with("+++ ")
                || raw_line.starts_with("diff --git")
                || raw_line.starts_with("index "))
        {
            continue;
        }

        let Some(hunk) = current.as_mut() else {
            continue; // Ignore stray text before any hunk header.
        };

        // Lenient: a truly empty line inside a hunk body is a blank context line.
        if raw_line.is_empty() {
            hunk.lines.push(HunkLine::Context(String::new()));
            continue;
        }
        match raw_line.as_bytes()[0] {
            b' ' => hunk
                .lines
                .push(HunkLine::Context(raw_line[1..].to_string())),
            b'-' => hunk
                .lines
                .push(HunkLine::Removal(raw_line[1..].to_string())),
            b'+' => hunk
                .lines
                .push(HunkLine::Addition(raw_line[1..].to_string())),
            _ => {
                return Err(FsError::DiffHunkHeader {
                    reason: format!("unrecognised hunk line prefix: {raw_line:?}"),
                });
            }
        }
    }
    if let Some(hunk) = current.take() {
        hunks.push(hunk);
    }
    Ok(hunks)
}

/// Parse the `-old_start,old_len +new_start,new_len @@` remainder of a hunk
/// header (the text after the leading `"@@ "` has already been stripped).
///
/// Why: Only `old_start` is needed to drive application (removal/context
/// lines are verified against actual content, so `old_len`/`new_*` are
/// informational only and are not validated here).
fn parse_hunk_header(header: &str) -> Result<usize, FsError> {
    let old_range = header
        .split_whitespace()
        .next()
        .and_then(|tok| tok.strip_prefix('-'))
        .ok_or_else(|| FsError::DiffHunkHeader {
            reason: format!("missing '-old_start' range in header: {header:?}"),
        })?;
    let old_start_str = old_range.split(',').next().unwrap_or(old_range);
    old_start_str
        .parse::<usize>()
        .map_err(|_| FsError::DiffHunkHeader {
            reason: format!("non-numeric old_start in header: {header:?}"),
        })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A single-hunk diff replaces one line and preserves the rest.
    #[test]
    fn apply_single_hunk() {
        let content = "line1\nline2\nline3\n";
        let diff = "@@ -2,1 +2,1 @@\n-line2\n+line2-changed\n";
        let updated = apply_unified_diff(content, diff, Path::new("f.py")).expect("must apply");
        assert_eq!(updated, "line1\nline2-changed\nline3\n");
    }

    /// Multiple hunks apply independently and in order.
    #[test]
    fn apply_multiple_hunks() {
        let content = "a\nb\nc\nd\ne\n";
        let diff = "@@ -1,1 +1,1 @@\n-a\n+A\n@@ -4,1 +4,1 @@\n-d\n+D\n";
        let updated = apply_unified_diff(content, diff, Path::new("f.py")).expect("must apply");
        assert_eq!(updated, "A\nb\nc\nD\ne\n");
    }

    /// A hunk with only additions (no removals) inserts without deleting.
    #[test]
    fn apply_pure_addition_hunk() {
        let content = "a\nb\n";
        let diff = "@@ -1,1 +1,2 @@\n a\n+inserted\n b\n";
        let updated = apply_unified_diff(content, diff, Path::new("f.py")).expect("must apply");
        assert_eq!(updated, "a\ninserted\nb\n");
    }

    /// A mismatched context/removal line surfaces `DiffContextMismatch`, not a panic.
    #[test]
    fn apply_context_mismatch_errors() {
        let content = "line1\nline2\nline3\n";
        let diff = "@@ -2,1 +2,1 @@\n-not-line2\n+x\n";
        let err = apply_unified_diff(content, diff, Path::new("f.py")).expect_err("must mismatch");
        assert!(matches!(err, FsError::DiffContextMismatch { .. }));
    }

    /// Diff text with no hunk header at all is a clear parse error.
    #[test]
    fn apply_no_hunks_errors() {
        let err = apply_unified_diff("content\n", "not a diff at all", Path::new("f.py"))
            .expect_err("must error");
        assert!(matches!(err, FsError::DiffHunkHeader { .. }));
    }

    /// An unparsable hunk header (non-numeric range) errors cleanly.
    #[test]
    fn apply_bad_header_errors() {
        let err = apply_unified_diff(
            "content\n",
            "@@ -x,y +1,1 @@\n-content\n+x\n",
            Path::new("f.py"),
        )
        .expect_err("must error on bad header");
        assert!(matches!(err, FsError::DiffHunkHeader { .. }));
    }

    /// Standard `--- a/file` / `+++ b/file` header lines before the first
    /// hunk are ignored rather than causing a parse error.
    #[test]
    fn apply_ignores_file_header_lines() {
        let content = "a\nb\n";
        let diff = "--- a/f.py\n+++ b/f.py\n@@ -1,1 +1,1 @@\n-a\n+A\n";
        let updated = apply_unified_diff(content, diff, Path::new("f.py")).expect("must apply");
        assert_eq!(updated, "A\nb\n");
    }
}
