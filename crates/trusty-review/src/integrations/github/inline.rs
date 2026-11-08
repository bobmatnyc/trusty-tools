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
//!     addresses), plus the old-side numbers of removed lines (`side: LEFT`)
//!     and the files the diff names (file-level anchors, #10762).
//!   * [`InlineComment`] is one would-be inline review comment (path/line/body).
//!   * [`InlinePlan`] is the full structured decision for a review: the inline
//!     comments to post plus the findings that fell back to the summary — surfaced
//!     verbatim in dry-run so the MCP response shows exactly what *would* be posted.
//!   * [`build_inline_plan`] is the pure mapping from findings → plan.
//!   * [`render_finding_comment`] renders one finding's inline-comment markdown,
//!     led by a verification caveat when the finding's own `verified` outcome
//!     owes the reader one (#5312) — a refuted claim must not read like a
//!     surviving one.
//!
//! Test: `inline_tests.rs` (sibling) — covers diff-index construction, the
//! finding→comment mapping, and off-diff fallback.

use std::collections::HashSet;

use crate::models::{
    AnchorSide, Effort, Finding, FindingCategory, SubjectType, UnanchoredFinding, UnanchoredReason,
    VerifyOutcome,
};

// ─── Tuning constants ─────────────────────────────────────────────────────────

/// Blockquote lead-in for the per-finding verification caveat (#5312).
///
/// Why: the summary banner already opens with `> **Verification notice:**`
/// (`pipeline::verification_notice`); using the same blockquote shape inline
/// means a reader meets one recognisable marker wherever the pipeline qualifies
/// a claim, and a distinct suffix keeps the two greppable apart.
/// What: prefixes the caveat returned by [`VerifyOutcome::reader_caveat`].
/// Test: `render_marks_refuted_finding`.
const VERIFICATION_CAVEAT_PREFIX: &str = "> **Verification:**";

/// Lead-in marking a style/preference finding as informational (#3474).
///
/// Why: `grade::derive_verdict_with` already refuses to let a style nit move the
/// verdict, but a reader meeting the comment on the diff cannot see that. Saying
/// it on the comment itself is what stops an author treating a taste note as a
/// change request.
/// What: prefixes the lead line of a [`crate::models::FindingCategory::Style`]
/// finding.
/// Test: `render_marks_style_finding_informational`,
/// `render_leaves_correctness_finding_unlabelled`.
const INFORMATIONAL_PREFIX: &str = "> **Informational (style / preference) — does not block.**";

/// Lead-in marking a coverage-gap finding as informational (#7036).
///
/// Why: `TestCoverage` became advisory in #7036, so it now earns the same
/// reader-facing label — but calling a missing test a "style / preference" nit
/// would misdescribe it, and the author would discount a note they should still
/// act on.  A distinct parenthetical keeps the "does not block" promise honest
/// without flattening why.
/// What: prefixes the lead line of a
/// [`crate::models::FindingCategory::TestCoverage`] finding.
/// Test: `render_marks_test_coverage_finding_informational`.
const COVERAGE_INFORMATIONAL_PREFIX: &str = "> **Informational (test coverage) — does not block.**";

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
/// #10762: also the old-side `(path, line)` of every removed line, and the
/// set of paths the diff names.
/// Test: `commentable_lines_indexes_added_and_context`,
/// `commentable_lines_excludes_removed_lines`,
/// `removed_line_with_no_new_side_match_is_not_off_diff`.
#[derive(Debug, Default, Clone)]
pub struct CommentableLines {
    anchors: HashSet<(String, u32)>,
    /// #10762: old-side numbers of removed lines, for a `LEFT` anchor.
    removed: HashSet<(String, u32)>,
    /// #10762: every path the diff names, for a file-level anchor.
    files: HashSet<String>,
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
    /// #10762: an old-side counter, seeded from `-a`, numbers each removed line
    /// into `removed`, keyed by the new path (the old path for a deleted file);
    /// every `+++ b/` path, and a deleted file's `--- a/` path, goes to `files`.
    /// Test: `commentable_lines_indexes_added_and_context`,
    /// `commentable_lines_excludes_removed_lines`,
    /// `commentable_lines_handles_multiple_hunks`,
    /// `removed_line_with_no_new_side_match_is_not_off_diff`.
    pub fn from_unified_diff(diff: &str) -> Self {
        let mut index = Self::default();
        let mut current_file: Option<String> = None;
        // #10762: `old_file` is the `--- a/` path; `removed_file` keys removed lines.
        let mut old_file: Option<String> = None;
        let mut removed_file: Option<String> = None;
        let mut new_line: u32 = 0;
        let mut old_line: Option<u32> = None;
        let mut in_hunk = false;

        for line in diff.lines() {
            if line.starts_with("diff ") {
                // #10762: a file header ends the hunk, so a following
                // `deleted file mode` line is never read as a context line.
                in_hunk = false;
                continue;
            }
            if let Some(path) = line.strip_prefix("+++ b/") {
                let path = path.trim().to_string();
                index.files.insert(path.clone());
                removed_file = Some(path.clone());
                current_file = Some(path);
                in_hunk = false;
                continue;
            }
            if line.starts_with("+++ ") {
                // e.g. `+++ /dev/null` — not a usable new-side path.
                current_file = None;
                // #10762: a deleted file's removed lines keep its old path.
                removed_file = old_file.clone();
                if let Some(path) = &old_file {
                    index.files.insert(path.clone());
                }
                in_hunk = false;
                continue;
            }
            if line.starts_with("--- ") {
                old_file = line.strip_prefix("--- a/").map(|p| p.trim().to_string());
                in_hunk = false;
                continue;
            }
            if let Some(rest) = line.strip_prefix("@@") {
                match parse_hunk_new_start(rest) {
                    Some(start) => {
                        new_line = start;
                        old_line = parse_hunk_old_start(rest);
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
            if line.starts_with("index ") || line.starts_with("\\ No newline") {
                continue;
            }
            if line.starts_with('-') {
                // Removed line: no new-side number, do not advance it.
                // #10762: record its old-side number for a LEFT anchor.
                if let Some(n) = old_line.as_mut() {
                    if let Some(file) = removed_file.as_ref() {
                        index.removed.insert((file.clone(), *n));
                    }
                    *n += 1;
                }
                continue;
            }
            let Some(file) = current_file.as_ref() else {
                continue;
            };
            index.anchors.insert((file.clone(), new_line));
            new_line += 1;
            // Added (`+`) or context line; only a context line has an old side.
            if !line.starts_with('+')
                && let Some(n) = old_line.as_mut()
            {
                *n += 1;
            }
        }

        index
    }

    /// Whether `(file, line)` is a removed line, by its old-side number (#10762).
    ///
    /// What: set-membership test against the removed-line index.
    /// Test: `removed_line_with_no_new_side_match_is_not_off_diff`.
    pub fn contains_removed(&self, file: &str, line: u32) -> bool {
        self.removed.contains(&(file.to_string(), line))
    }

    /// Whether the diff names `file` (#10762).
    ///
    /// What: set-membership test against the diff's paths.
    /// Test: `file_level_anchor_for_in_diff_file_without_line`.
    pub fn contains_file(&self, file: &str) -> bool {
        self.files.contains(file)
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

/// Parse the old-side start line from the remainder of an `@@` hunk header (#10762).
///
/// Why: a `LEFT` anchor addresses a removed line by its old-side number, which
/// counts from `-a` in `@@ -a,b +c,d @@`.
/// What: strips leading spaces and the `-`, then parses the integer before an
/// optional `,count`. Returns `None` for a malformed header; removed lines in
/// that hunk are then not indexed.
/// Test: `deleted_line_finding_anchors_left`.
fn parse_hunk_old_start(rest: &str) -> Option<u32> {
    let after = rest.trim_start().strip_prefix('-')?;
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
/// What: `path` + `line` + `side` are the anchor; `body` is the rendered markdown.
/// #10762: `side` is `LEFT` for a removed line; `subject_type` is `File`, with
/// `line` 0, for a file-level anchor.
/// Test: `build_inline_plan_maps_on_diff_finding`, `deleted_line_finding_anchors_left`,
/// `file_level_anchor_for_in_diff_file_without_line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineComment {
    /// Changed file path (new side), e.g. `src/db.rs`.
    pub path: String,
    /// Line number the comment anchors to, on `side`; 0 for a file-level anchor.
    pub line: u32,
    /// #10762: the diff side `line` counts on.
    pub side: AnchorSide,
    /// #10762: `Some(File)` for a file-level anchor, else `None`.
    pub subject_type: Option<SubjectType>,
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
/// is the count of low-severity findings rolled up past the inline cap (#1420);
/// `unanchored` names every finding not placed inline, with its reason
/// (#10762), so `inline_indices` and `unanchored` partition the input.
/// Test: `build_inline_plan_*` tests in `inline_tests.rs`,
/// `unanchored_reason_enum_covers_no_line_off_diff_nit_cap_withheld`.
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
    /// #10762: every finding not placed inline, indexed into the input slice.
    pub unanchored: Vec<UnanchoredFinding>,
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
/// preserved. A `citation_partial` finding always goes to the summary (#8949).
/// #10762: each finding not placed inline is recorded in `unanchored` as
/// `no_line`, `off_diff` (a `citation_partial` finding included), `deleted_line`
/// or `nit_cap`. Placement is decided by `place_finding`.
/// Test: `build_inline_plan_maps_on_diff_finding`,
/// `build_inline_plan_off_diff_falls_back`, `build_inline_plan_no_line_falls_back`,
/// `nit_cap_rolls_up_overflow`, `nit_cap_does_not_suppress_high_severity`,
/// `build_inline_plan_posts_a_partial_citation_in_the_body`,
/// `unanchored_reason_enum_covers_no_line_off_diff_nit_cap_withheld`,
/// `deleted_line_finding_anchors_left`,
/// `file_level_anchor_for_in_diff_file_without_line`.
pub fn build_inline_plan(findings: &[Finding], commentable: &CommentableLines) -> InlinePlan {
    let mut plan = InlinePlan::default();
    let mut inline_nits = 0usize;

    for (idx, finding) in findings.iter().enumerate() {
        let anchor = match place_finding(finding, commentable) {
            Ok(anchor) => anchor,
            Err(reason) => {
                // No usable diff anchor → summary body (never fails the post).
                plan.summary_findings.push(finding.clone());
                plan.unanchored.push(UnanchoredFinding {
                    finding_index: idx,
                    reason,
                });
                continue;
            }
        };

        // Nit-volume cap (#1420): only Effort::Low findings are subject to it.
        if finding.effort == Effort::Low {
            if inline_nits >= MAX_INLINE_NITS {
                plan.suppressed_nits += 1;
                // #10762: the overflow nit is named, not only counted.
                plan.unanchored.push(UnanchoredFinding {
                    finding_index: idx,
                    reason: UnanchoredReason::NitCap,
                });
                continue;
            }
            inline_nits += 1;
        }

        plan.comments.push(InlineComment {
            path: finding.file.clone(),
            line: anchor.line,
            side: anchor.side,
            subject_type: anchor.subject_type,
            body: render_finding_comment(finding),
        });
        // Record identity (not coordinate) of the inline-placed finding so the
        // summary body can exclude exactly these — never a same-anchor sibling.
        plan.inline_indices.push(idx);
    }

    plan
}

/// Where a finding anchors in the diff (#10762).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    /// Line on `side`; 0 for a file-level anchor.
    line: u32,
    /// The diff side `line` counts on.
    side: AnchorSide,
    /// `Some(File)` for a file-level anchor.
    subject_type: Option<SubjectType>,
}

/// Decide one finding's anchor, or why it has none (#10762).
///
/// Why: a finding at a deleted line read as `off_diff`, and a finding with a
/// file but no line read as `no_line`, though GitHub can anchor both.
/// What: with a line, the new side is tried first (`RIGHT`); a removed line
/// with no new-side match anchors `LEFT` at its old-side number. With no line,
/// a file the diff names gets a file-level anchor. A `citation_partial`
/// finding is never placed (#8949): it reads `deleted_line` when its line is
/// only a removed line, else `off_diff` or `no_line`.
/// Test: `deleted_line_finding_anchors_left`,
/// `removed_line_with_no_new_side_match_is_not_off_diff`,
/// `file_level_anchor_for_in_diff_file_without_line`,
/// `no_line_and_file_not_in_diff_is_unanchored_no_line`.
fn place_finding(
    finding: &Finding,
    commentable: &CommentableLines,
) -> Result<Anchor, UnanchoredReason> {
    let file = finding.file.as_str();
    let Some(line) = finding.line else {
        if finding.citation_partial || !commentable.contains_file(file) {
            return Err(UnanchoredReason::NoLine);
        }
        return Ok(Anchor {
            line: 0,
            side: AnchorSide::Right,
            subject_type: Some(SubjectType::File),
        });
    };
    let new_side = commentable.contains(file, line);
    let removed_only = !new_side && commentable.contains_removed(file, line);
    if finding.citation_partial {
        // #8949: a partly verified citation is posted in the body, never inline.
        return Err(if removed_only {
            UnanchoredReason::DeletedLine
        } else {
            UnanchoredReason::OffDiff
        });
    }
    let side = match (new_side, removed_only) {
        (true, _) => AnchorSide::Right,
        (false, true) => AnchorSide::Left,
        (false, false) => return Err(UnanchoredReason::OffDiff),
    };
    Ok(Anchor {
        line,
        side,
        subject_type: None,
    })
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
/// replacement degrades to prose, never a broken suggestion block.  When the
/// finding's own `verified` outcome owes the reader a caveat
/// ([`VerifyOutcome::reader_caveat`]) that caveat is prepended as a blockquote
/// ABOVE the claim, so a refuted or unverified finding can never read as a
/// surviving one (#5312).
/// Test: `render_finding_comment_includes_kind_and_fix`,
/// `render_emits_suggestion_block`, `render_falls_back_to_prose_fix`,
/// `render_includes_consequence`, `low_confidence_finding_is_hedged`,
/// `render_marks_refuted_finding`, `render_marks_error_refuted_as_unverified`,
/// `render_leaves_confirmed_and_unjudged_findings_unmarked`,
/// `render_marks_style_finding_informational` (#3474),
/// `render_marks_test_coverage_finding_informational` (#7036),
/// `render_leaves_correctness_finding_unlabelled` (#3474).
pub fn render_finding_comment(finding: &Finding) -> String {
    let mut out = String::with_capacity(256);

    // #3474: say up front that an advisory finding is advisory, so the reader is
    // never left inferring severity from a comment the verdict already
    // discounted.  #7036: name WHICH kind of advisory, so a coverage gap is not
    // mislabelled a taste note.
    if finding.category.is_informational() {
        out.push_str(if finding.category == FindingCategory::TestCoverage {
            COVERAGE_INFORMATIONAL_PREFIX
        } else {
            INFORMATIONAL_PREFIX
        });
        out.push_str("\n\n");
    }

    // #5312: a finding the pipeline does not stand behind must say so before it
    // says anything else — keyed on the finding's own recorded outcome, never on
    // a per-surface list of outcomes someone remembered to enumerate here.
    if let Some(caveat) = finding
        .verified
        .as_ref()
        .and_then(VerifyOutcome::reader_caveat)
    {
        out.push_str(&format!("{VERIFICATION_CAVEAT_PREFIX} {caveat}\n\n"));
    }

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
