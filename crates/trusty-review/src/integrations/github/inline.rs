//! Inline per-line PR review comments (#1414).
//!
//! Why: posting all findings concatenated into one PR-level summary comment
//! buries actionable feedback — the author has to map each finding back to the
//! line it is about by hand.  GitHub's "pull request review" API accepts a
//! `comments[]` array where each entry is anchored to a `path` + `line` in the
//! diff, so the reviewer's findings can land *exactly* on the offending line as
//! a normal inline review comment.  This module turns a review's `Finding`s into
//! that `comments[]` payload, with a robust fallback: a finding whose line is not
//! part of the PR diff (GitHub rejects off-diff anchors) is rolled into the
//! summary body instead of failing the whole review.
//!
//! What:
//!   * [`CommentableLines`] indexes which `(file, line)` pairs are valid inline
//!     anchors by parsing the unified diff's `@@` hunks (added + context lines on
//!     the new/right side, which is what the GitHub `line`/`side: RIGHT` anchor
//!     addresses).
//!   * [`InlineComment`] is one would-be inline review comment (path/line/body).
//!   * [`InlinePlan`] is the full structured decision for a review: the inline
//!     comments to post plus the findings that fell back to the summary — surfaced
//!     verbatim in dry-run so the MCP response shows exactly what *would* be posted.
//!   * [`build_inline_plan`] is the pure mapping from findings → plan.
//!   * [`render_finding_comment`] renders one finding's inline-comment markdown.
//!
//! Test: `inline_tests.rs` (sibling) — covers diff-index construction, the
//! finding→comment mapping, and off-diff fallback.

use std::collections::HashSet;

use crate::models::Finding;

// ─── Commentable-line index ───────────────────────────────────────────────────

/// Index of `(file, new-side line)` pairs that are valid inline-comment anchors.
///
/// Why: GitHub rejects a review comment whose `line` is not part of the PR diff;
/// posting such a comment fails the whole `POST /pulls/{n}/reviews` call.  We must
/// therefore know, *before* building the payload, which lines are anchorable so
/// off-diff findings can be diverted to the summary body instead of breaking the
/// post.  The valid anchors are the added (`+`) and context (` `) lines on the new
/// side of each hunk — these are the lines GitHub's `side: RIGHT` anchor addresses.
/// What: a set of `(path, line)` pairs parsed from the unified diff's `@@` headers.
/// Test: `commentable_lines_indexes_added_and_context`,
/// `commentable_lines_excludes_removed_lines`.
#[derive(Debug, Default, Clone)]
pub struct CommentableLines {
    anchors: HashSet<(String, u32)>,
}

impl CommentableLines {
    /// Parse a unified diff into the set of commentable new-side line anchors.
    ///
    /// Why: this is the single source of truth for "can a comment land on this
    /// line?"; centralising the unified-diff hunk walk keeps the fallback logic in
    /// [`build_inline_plan`] trivial (a set membership test).
    /// What: walks the diff line by line, tracking the current file (from `+++ b/`
    /// headers) and the new-side line counter (seeded from each `@@ -a,b +c,d @@`
    /// header's `+c`).  Added (`+`) and context (` `) lines advance the new-side
    /// counter and are recorded as anchors; removed (`-`) lines do not (they have
    /// no new-side line number).  Malformed `@@` headers are skipped defensively.
    /// Test: `commentable_lines_indexes_added_and_context`,
    /// `commentable_lines_excludes_removed_lines`,
    /// `commentable_lines_handles_multiple_hunks`.
    pub fn from_unified_diff(diff: &str) -> Self {
        let mut anchors: HashSet<(String, u32)> = HashSet::new();
        let mut current_file: Option<String> = None;
        let mut new_line: u32 = 0;
        let mut in_hunk = false;

        for line in diff.lines() {
            if let Some(path) = line.strip_prefix("+++ b/") {
                current_file = Some(path.trim().to_string());
                in_hunk = false;
                continue;
            }
            if line.starts_with("+++ ") {
                // e.g. `+++ /dev/null` — not a usable new-side path.
                current_file = None;
                in_hunk = false;
                continue;
            }
            if line.starts_with("--- ") {
                in_hunk = false;
                continue;
            }
            if let Some(rest) = line.strip_prefix("@@") {
                match parse_hunk_new_start(rest) {
                    Some(start) => {
                        new_line = start;
                        in_hunk = true;
                    }
                    None => in_hunk = false,
                }
                continue;
            }
            if !in_hunk {
                continue;
            }
            // Diff-file metadata lines that can appear between hunks.
            if line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("\\ No newline")
            {
                continue;
            }
            let Some(file) = current_file.as_ref() else {
                continue;
            };
            match line.chars().next() {
                Some('+') => {
                    anchors.insert((file.clone(), new_line));
                    new_line += 1;
                }
                Some('-') => {
                    // Removed line: no new-side number, do not advance.
                }
                _ => {
                    // Context line (leading space or empty): anchorable, advances.
                    anchors.insert((file.clone(), new_line));
                    new_line += 1;
                }
            }
        }

        Self { anchors }
    }

    /// Return whether `(file, line)` is a valid inline-comment anchor.
    ///
    /// Why: [`build_inline_plan`] uses this to decide inline vs. summary fallback.
    /// What: set-membership test against the parsed anchors.
    /// Test: `commentable_lines_indexes_added_and_context`.
    pub fn contains(&self, file: &str, line: u32) -> bool {
        self.anchors.contains(&(file.to_string(), line))
    }

    /// Number of indexed anchors (used in tests / telemetry).
    ///
    /// Why: lets tests assert the index size without exposing the inner set.
    /// What: returns the anchor count.
    /// Test: `commentable_lines_indexes_added_and_context`.
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Whether the index is empty (no commentable lines parsed).
    ///
    /// Why: clippy requires `is_empty` alongside `len`; also a quick "no diff
    /// positions available" check for callers.
    /// What: returns `true` when no anchors were parsed.
    /// Test: covered transitively by `commentable_lines_indexes_added_and_context`.
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// Parse the new-side start line from the remainder of an `@@` hunk header.
///
/// Why: the new-side line counter must be seeded from `+c` in `@@ -a,b +c,d @@`
/// so anchors map to the right (post-change) line numbers GitHub expects.
/// What: finds the `+` token, parses the integer before an optional `,count`.
/// Returns `None` for a malformed header (caller then skips the hunk).
/// Test: `parse_hunk_new_start_basic`, `parse_hunk_new_start_without_count`.
fn parse_hunk_new_start(rest: &str) -> Option<u32> {
    let plus = rest.find('+')?;
    let after = &rest[plus + 1..];
    let token: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    token.parse::<u32>().ok()
}

// ─── Inline comment + plan types ──────────────────────────────────────────────

/// One would-be inline review comment anchored to a diff line.
///
/// Why: the GitHub review payload needs `{ path, line, body }` per inline comment;
/// modelling it explicitly lets the dry-run path surface the exact set that would
/// be posted (so the MCP response is faithful) and keeps the live POST a trivial
/// serialisation.
/// What: `path` + `line` are the new-side anchor; `body` is the rendered markdown.
/// Test: `build_inline_plan_maps_on_diff_finding`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineComment {
    /// Changed file path (new side), e.g. `src/db.rs`.
    pub path: String,
    /// New-side line number the comment anchors to.
    pub line: u32,
    /// Rendered comment markdown body.
    pub body: String,
}

/// The full inline-posting decision for one review.
///
/// Why: separating the *decision* (which comments go inline, which fall back) from
/// the *side effect* (the POST) makes the mapping unit-testable with no network,
/// and lets dry-run render the same structure the live path would post.
/// What: `comments` are the inline comments to attach to the review;
/// `summary_findings` are findings that could not be anchored inline (off-diff or
/// no line) and must be rendered into the review summary body.
/// Test: `build_inline_plan_*` tests in `inline_tests.rs`.
#[derive(Debug, Clone, Default)]
pub struct InlinePlan {
    /// Inline comments to post (each anchored to a diff line).
    pub comments: Vec<InlineComment>,
    /// Findings that fall back to the summary body (no anchorable line).
    pub summary_findings: Vec<Finding>,
}

// ─── Mapping: findings → inline plan ──────────────────────────────────────────

/// Map a review's findings to an [`InlinePlan`] against the diff's commentable lines.
///
/// Why: this is the heart of #1414 — it decides, per finding, whether it can land
/// inline (its `(file, line)` is in the diff) or must fall back to the summary
/// body, so an off-diff finding never fails the whole post.
/// What: iterates findings; a finding with a `line` that is a commentable anchor
/// becomes an [`InlineComment`] (body via [`render_finding_comment`]); a finding
/// with no line, or an off-diff line, goes to `summary_findings`.  Ordering of
/// inputs is preserved.
/// Test: `build_inline_plan_maps_on_diff_finding`,
/// `build_inline_plan_off_diff_falls_back`, `build_inline_plan_no_line_falls_back`.
pub fn build_inline_plan(findings: &[Finding], commentable: &CommentableLines) -> InlinePlan {
    let mut plan = InlinePlan::default();

    for finding in findings {
        let anchor = finding
            .line
            .filter(|l| commentable.contains(&finding.file, *l));

        match anchor {
            Some(line) => plan.comments.push(InlineComment {
                path: finding.file.clone(),
                line,
                body: render_finding_comment(finding),
            }),
            None => plan.summary_findings.push(finding.clone()),
        }
    }

    plan
}

// ─── Per-finding comment rendering ────────────────────────────────────────────

/// Render one finding as inline-comment markdown.
///
/// Why: an inline comment must read as a single, self-contained, actionable note:
/// a clear lead line and the proposed fix.  Centralising the rendering keeps the
/// inline and (future) summary renderings consistent.
/// What: builds `**<kind>** — <description>` followed by a `_Fix:_ <suggestion>`
/// line when the finding carries a suggestion.
/// Test: `render_finding_comment_includes_kind_and_fix`.
pub fn render_finding_comment(finding: &Finding) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(&format!(
        "**{}** — {}\n",
        finding.kind,
        finding.description.trim()
    ));
    let suggestion = finding.suggestion.trim();
    if !suggestion.is_empty() {
        out.push_str(&format!("\n_Fix:_ {suggestion}\n"));
    }
    out
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "inline_tests.rs"]
mod tests;
