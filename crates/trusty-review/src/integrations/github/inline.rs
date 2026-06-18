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

use crate::models::{Effort, Finding};

// ─── Tuning constants ─────────────────────────────────────────────────────────

/// Confidence below which a finding is hedged rather than asserted (#1416).
///
/// Why: an automated reviewer that asserts a low-confidence guess as fact erodes
/// author trust; hedging ("This may…") signals the uncertainty honestly without
/// dropping the finding.  0.6 is the midpoint between "plausible" and "likely" on
/// the LLM's own 0.0–1.0 confidence scale.
/// What: findings with `confidence < UNCERTAINTY_THRESHOLD` get a hedging prefix.
/// Test: `low_confidence_finding_is_hedged`, `high_confidence_finding_is_asserted`.
pub const UNCERTAINTY_THRESHOLD: f32 = 0.6;

/// Maximum low-severity (nit) findings posted inline before the rest roll up (#1420).
///
/// Why: a review flooded with low-severity nits trains authors to ignore the feed
/// and buries the real issues (the principles layer's "keep signal-to-noise high"
/// rule).  Capping inline nits at a small N and rolling the overflow into one
/// summary line keeps the inline feed high-signal.
/// What: at most `MAX_INLINE_NITS` `Effort::Low` findings are emitted inline; the
/// remainder increment [`InlinePlan::suppressed_nits`].  Higher-severity findings
/// are never subject to the cap.
/// Test: `nit_cap_rolls_up_overflow`, `nit_cap_keeps_first_n_inline`,
/// `nit_cap_does_not_suppress_high_severity`.
pub const MAX_INLINE_NITS: usize = 5;

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
/// no line) and must be rendered into the review summary body; `inline_indices`
/// records the index (into the original `findings` slice) of every finding that
/// became an inline comment, so downstream code can partition by finding identity
/// rather than by `(file, line)` coordinate (two distinct findings can share the
/// same anchor — coordinate matching silently drops one); `suppressed_nits`
/// is the count of low-severity findings rolled up past the inline cap (#1420).
/// Test: `build_inline_plan_*` tests in `inline_tests.rs`.
#[derive(Debug, Clone, Default)]
pub struct InlinePlan {
    /// Inline comments to post (each anchored to a diff line).
    pub comments: Vec<InlineComment>,
    /// Findings that fall back to the summary body (no anchorable line).
    pub summary_findings: Vec<Finding>,
    /// Indices (into the input `findings` slice) of findings placed inline.
    ///
    /// Why: the summary body must render exactly the findings that did NOT land
    /// inline.  Identifying those by `(file, line)` coordinate is lossy — two
    /// distinct findings at the same anchor collide and one is silently dropped
    /// from both the inline set and the summary.  Carrying the authoritative
    /// inline index set makes the partition by identity, never by coordinate.
    pub inline_indices: Vec<usize>,
    /// Count of low-severity nits suppressed past the inline cap (#1420).
    pub suppressed_nits: usize,
}

impl InlinePlan {
    /// One-line rollup sentence for suppressed nits, or `None` when none (#1420).
    ///
    /// Why: overflow nits must be acknowledged in the summary so the author knows
    /// feedback was withheld for signal — silently dropping them is worse than one
    /// honest line.
    /// What: returns e.g. `"+7 more minor nits suppressed to keep this review focused."`
    /// when `suppressed_nits > 0`, else `None`.
    /// Test: `nit_cap_rolls_up_overflow`.
    pub fn suppressed_nits_line(&self) -> Option<String> {
        if self.suppressed_nits == 0 {
            None
        } else {
            Some(format!(
                "+{} more minor nit{} suppressed to keep this review focused.",
                self.suppressed_nits,
                if self.suppressed_nits == 1 { "" } else { "s" }
            ))
        }
    }
}

// ─── Mapping: findings → inline plan ──────────────────────────────────────────

/// Map a review's findings to an [`InlinePlan`] against the diff's commentable lines.
///
/// Why: this is the heart of #1414 + #1420 — it decides, per finding, whether it
/// can land inline (its `(file, line)` is in the diff) or must fall back to the
/// summary body, and it enforces the nit-volume cap so low-severity findings cannot
/// flood the inline feed.
/// What: iterates findings; a finding with a `line` that is a commentable anchor
/// becomes an [`InlineComment`] (body via [`render_finding_comment`]); a finding
/// with no line, or an off-diff line, goes to `summary_findings`.  Among the
/// anchorable findings, `Effort::Low` (nit) findings beyond [`MAX_INLINE_NITS`]
/// inline placements are not emitted inline — they increment `suppressed_nits`
/// instead.  Higher-severity findings are never suppressed.  Input order is
/// preserved.
/// Test: `build_inline_plan_maps_on_diff_finding`,
/// `build_inline_plan_off_diff_falls_back`, `build_inline_plan_no_line_falls_back`,
/// `nit_cap_rolls_up_overflow`, `nit_cap_does_not_suppress_high_severity`.
pub fn build_inline_plan(findings: &[Finding], commentable: &CommentableLines) -> InlinePlan {
    let mut plan = InlinePlan::default();
    let mut inline_nits = 0usize;

    for (idx, finding) in findings.iter().enumerate() {
        let anchor = finding
            .line
            .filter(|l| commentable.contains(&finding.file, *l));

        let Some(line) = anchor else {
            // No usable diff anchor → summary body (never fails the post).
            plan.summary_findings.push(finding.clone());
            continue;
        };

        // Nit-volume cap (#1420): only Effort::Low findings are subject to it.
        if finding.effort == Effort::Low {
            if inline_nits >= MAX_INLINE_NITS {
                plan.suppressed_nits += 1;
                continue;
            }
            inline_nits += 1;
        }

        plan.comments.push(InlineComment {
            path: finding.file.clone(),
            line,
            body: render_finding_comment(finding),
        });
        // Record identity (not coordinate) of the inline-placed finding so the
        // summary body can exclude exactly these — never a same-anchor sibling.
        plan.inline_indices.push(idx);
    }

    plan
}

// ─── Per-finding comment rendering ────────────────────────────────────────────

/// Render one finding as inline-comment markdown.
///
/// Why: an inline comment must read as a single, self-contained, actionable note:
/// a clear lead line, *why it matters* (the consequence, #1416), and the fix as a
/// *committable* GitHub ```suggestion block when the finding carries concrete
/// replacement code (#1415) or as prose otherwise.  Low-confidence findings are
/// hedged rather than asserted (#1416) so the reviewer never overstates a guess.
/// Centralising the rendering keeps the inline and (future) summary renderings
/// consistent.
/// What: builds `**<kind>** — <description>` (hedged with "This may…" when
/// `confidence < UNCERTAINTY_THRESHOLD`), appends a `_Why it matters:_ <consequence>`
/// line when the finding carries a consequence, then appends either a fenced
/// ```suggestion block (when [`suggestion_replacement`] accepts the finding's
/// `suggested_replacement`) or a `_Fix:_ <prose>` line.  A malformed or non-code
/// replacement degrades to prose, never a broken suggestion block.
/// Test: `render_finding_comment_includes_kind_and_fix`,
/// `render_emits_suggestion_block`, `render_falls_back_to_prose_fix`,
/// `render_includes_consequence`, `low_confidence_finding_is_hedged`.
pub fn render_finding_comment(finding: &Finding) -> String {
    let mut out = String::with_capacity(256);

    // Lead line: kind + description, hedged for low confidence (#1416).
    let hedged = if finding.confidence < UNCERTAINTY_THRESHOLD {
        hedge_description(&finding.description)
    } else {
        finding.description.trim().to_string()
    };
    out.push_str(&format!("**{}** — {}\n", finding.kind, hedged));

    // Consequence: what goes wrong if unaddressed (#1416).
    let consequence = finding.consequence.trim();
    if !consequence.is_empty() {
        out.push_str(&format!("\n_Why it matters:_ {consequence}\n"));
    }

    // Fix: committable suggestion block when concrete (#1415), else prose.
    if let Some(replacement) = suggestion_replacement(finding) {
        out.push_str("\n```suggestion\n");
        out.push_str(&replacement);
        out.push_str("\n```\n");
    } else {
        let suggestion = finding.suggestion.trim();
        if !suggestion.is_empty() {
            out.push_str(&format!("\n_Fix:_ {suggestion}\n"));
        }
    }
    out
}

/// Prefix a description with a hedging phrase when not already hedged (#1416).
///
/// Why: low-confidence findings should *read* as tentative; mechanically prefixing
/// a hedge is more reliable than asking the LLM to self-hedge.
/// What: if the description already opens with a hedging word (may/might/possibly/
/// could/perhaps), returns it trimmed unchanged; otherwise lowercases the first
/// letter and prepends "This may be an issue: ".  Empty descriptions pass through.
/// Test: `low_confidence_finding_is_hedged`, `already_hedged_not_double_hedged`.
fn hedge_description(description: &str) -> String {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    const HEDGES: &[&str] = &[
        "may ", "might ", "possibly", "could ", "perhaps", "this may",
    ];
    if HEDGES.iter().any(|h| lower.starts_with(h)) {
        return trimmed.to_string();
    }
    let mut chars = trimmed.chars();
    let first = chars.next().unwrap_or(' ').to_ascii_lowercase();
    let rest: String = chars.collect();
    format!("This may be an issue: {first}{rest}")
}

/// Extract a committable replacement block from a finding's `suggested_replacement`.
///
/// Why: a GitHub ```suggestion block must contain the *exact* replacement lines —
/// not prose, not a multi-hunk patch.  Emitting prose inside a suggestion fence
/// produces a broken one-click-apply that corrupts the file, so we only emit the
/// block when the structured field looks like a safe, fence-free replacement and
/// otherwise degrade to a prose `_Fix:_` line.
/// What: returns `Some(lines)` when the finding's `suggested_replacement` is
/// present and [`suggestion_is_committable`] accepts it, else `None`.  The returned
/// string is the verbatim replacement text (leading/trailing blank lines trimmed).
/// Test: `render_emits_suggestion_block`, `render_falls_back_to_prose_fix`,
/// `suggestion_with_fence_is_rejected`.
fn suggestion_replacement(finding: &Finding) -> Option<String> {
    let raw = finding.suggested_replacement.as_deref()?;
    let s = raw.trim_matches('\n');
    if suggestion_is_committable(s) {
        Some(s.to_string())
    } else {
        None
    }
}

/// Decide whether a replacement string is a safe committable suggestion block.
///
/// Why: malformed replacements must degrade to prose, never break posting — a
/// triple-backtick fence inside the replacement would close the ```suggestion
/// block early, and an empty replacement is not committable code.
/// What: rejects empty strings and any string containing a ``` fence; accepts
/// everything else as a literal replacement block.
/// Test: `suggestion_with_fence_is_rejected`, `code_suggestion_is_committable`.
pub fn suggestion_is_committable(suggestion: &str) -> bool {
    let s = suggestion.trim();
    if s.is_empty() {
        return false;
    }
    // A ``` fence inside the replacement would prematurely close the block.
    if s.contains("```") {
        return false;
    }
    true
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "inline_tests.rs"]
mod tests;
