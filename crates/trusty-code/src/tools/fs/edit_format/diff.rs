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
/// `content`'s lines, and returns the full patched text. Returns
/// `FsError::DiffHunkHeader` if no valid hunk header is found, or
/// `FsError::DiffContextMismatch` if a context/removal line does not match
/// the file at the expected position.
///
/// Trailing-newline semantics (#2150, hardened after a #2975 code-critic
/// HIGH finding): the default is to preserve whether the ORIGINAL `content`
/// ended with `\n`. That default is overridden ONLY when the LAST hunk
/// actually reaches end-of-file (no untouched original lines remain after
/// it) AND that hunk carries a `\ No newline at end of file` footer marker —
/// see [`Hunk`]'s docs for how the marker's position (after a `-`/old-side,
/// `+`/new-side, or ` `/both-sides line) is tracked. A marker attached to the
/// new/both side means the OUTPUT has no trailing newline; a marker attached
/// ONLY to the old side means the diff is REMOVING a no-trailing-newline
/// state, so the output DOES get a trailing newline, regardless of what the
/// original file had. No marker at all in the tail-reaching hunk leaves the
/// default (original file's own state) untouched.
/// Test: `tests::apply_single_hunk`, `tests::apply_multiple_hunks`,
/// `tests::apply_context_mismatch_errors`, `tests::apply_no_hunks_errors`,
/// `tests::apply_footer_marker_after_removal_only_forces_trailing_newline`,
/// `tests::apply_footer_marker_after_addition_removes_trailing_newline`,
/// `tests::apply_footer_marker_on_both_sides_keeps_no_trailing_newline`.
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

    // Set only when the LAST hunk reaches end-of-file and carries an
    // explicit `\ No newline at end of file` hint; see the doc above.
    let mut trailing_newline_override: Option<bool> = None;
    let last_hunk_idx = hunks.len() - 1;

    for (idx, hunk) in hunks.iter().enumerate() {
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

        if idx == last_hunk_idx && cursor == orig_lines.len() {
            trailing_newline_override = match (hunk.new_tail_no_newline, hunk.old_tail_no_newline) {
                // New/both side explicitly marked: output has no trailing \n.
                (true, _) => Some(false),
                // Only the old side was marked: the diff REMOVES the
                // no-trailing-newline state, so output DOES get one.
                (false, true) => Some(true),
                // No marker at all: leave the original file's own state.
                (false, false) => None,
            };
        }
    }
    out.extend(orig_lines[cursor..].iter().map(|s| s.to_string()));

    let mut result = out.join("\n");
    if trailing_newline_override.unwrap_or(had_trailing_newline) {
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
///
/// Why (#2150, hardened after a #2975 code-critic HIGH finding): a `git
/// diff`-style `\ No newline at end of file` footer marker's POSITION
/// carries meaning, not just its presence. It always immediately follows one
/// hunk-body line, and which line tells you which file(s) it describes:
/// after a `-` (`Removal`) line, the OLD file's last line there has no
/// trailing newline; after a `+` (`Addition`) line, the NEW file's last line
/// there has no trailing newline; after a ` ` (`Context`) line — common to
/// both files — BOTH lack one. `old_tail_no_newline` / `new_tail_no_newline`
/// record which side(s) any footer marker(s) in this hunk were attached to,
/// so [`apply_unified_diff`] can decide the OUTPUT's trailing-newline state
/// correctly instead of blindly copying the original file's.
/// What: `old_tail_no_newline` is `true` when a footer followed a `Removal`
/// or `Context` line in this hunk; `new_tail_no_newline` is `true` when one
/// followed an `Addition` or `Context` line. Both can be `true` at once (two
/// separate markers, one per side, both lacking a trailing newline with
/// different content).
#[derive(Debug, Clone, PartialEq)]
struct Hunk {
    old_start: usize,
    lines: Vec<HunkLine>,
    old_tail_no_newline: bool,
    new_tail_no_newline: bool,
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
                old_tail_no_newline: false,
                new_tail_no_newline: false,
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
            // #2150: `git diff`-style output emits a footer marker line —
            // `\ No newline at end of file` — directly after the last
            // context/removal/addition line of a hunk when that line lacked a
            // trailing newline in the source. It is metadata about the
            // preceding line, not a hunk-body line itself, so it carries no
            // content to apply — but WHICH line it followed matters (see
            // `Hunk`'s docs), so record that before moving on.
            b'\\' => {
                match hunk.lines.last() {
                    Some(HunkLine::Context(_)) => {
                        hunk.old_tail_no_newline = true;
                        hunk.new_tail_no_newline = true;
                    }
                    Some(HunkLine::Removal(_)) => hunk.old_tail_no_newline = true,
                    Some(HunkLine::Addition(_)) => hunk.new_tail_no_newline = true,
                    // A footer marker with no preceding line in this hunk is
                    // malformed input we can't attribute to a side; ignore it
                    // rather than erroring — the marker carries no content of
                    // its own to apply either way.
                    None => {}
                }
                continue;
            }
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

    /// #2150/#2975: a footer marker attached ONLY to the removed (old-side)
    /// line means the diff is REMOVING a no-trailing-newline state — the
    /// output must gain a trailing newline even though the original file
    /// lacked one. (Regression case: pre-fix this incorrectly copied the
    /// original file's own missing-trailing-newline state into the output.)
    #[test]
    fn apply_footer_marker_after_removal_only_forces_trailing_newline() {
        let content = "a\nb"; // no trailing newline
        let diff = "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+B\n";
        let updated = apply_unified_diff(content, diff, Path::new("f.py")).expect("must apply");
        assert_eq!(updated, "a\nB\n");
    }

    /// #2150/#2975: a footer marker attached to the added (new-side) line
    /// means the diff INTRODUCES a no-trailing-newline state — the output
    /// must lack a trailing newline even though the original file had one.
    #[test]
    fn apply_footer_marker_after_addition_removes_trailing_newline() {
        let content = "a\nb\n"; // has a trailing newline
        let diff = "@@ -1,2 +1,2 @@\n a\n-b\n+B\n\\ No newline at end of file\n";
        let updated = apply_unified_diff(content, diff, Path::new("f.py")).expect("must apply");
        assert_eq!(updated, "a\nB");
    }

    /// #2150/#2975: footer markers on BOTH sides (old line lacked a trailing
    /// newline, and so does its new-side replacement) leave the
    /// no-trailing-newline state unchanged across the edit.
    #[test]
    fn apply_footer_marker_on_both_sides_keeps_no_trailing_newline() {
        let content = "a\nb"; // no trailing newline
        let diff = "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+B\n\\ No newline at end of file\n";
        let updated = apply_unified_diff(content, diff, Path::new("f.py")).expect("must apply");
        assert_eq!(updated, "a\nB");
    }
}
