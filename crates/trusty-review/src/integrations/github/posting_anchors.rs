//! The posting guard for inline anchors (#10762).
//!
//! Why: GitHub's review POST `comments[]` takes `path`, `line`, `side`,
//! `start_line`, `start_side` and `body`, but no `subject_type`. A
//! file-level anchor (`line: 0`) there is a certain 422, and the review POST
//! is atomic, so one such entry would drop the whole review.
//! What: [`posting_view`] moves every file-level or `line: 0` entry out of
//! the inline set and into the summary, on a copy used only for the POST;
//! [`comment_json`] renders one entry with its real side and range.
//! Test: `post_payload_uses_left_side_and_start_line`,
//! `file_level_and_line_zero_entries_demoted_to_summary_not_posted_inline`,
//! `demoted_entry_rendered_exactly_once_in_summary`.

use std::borrow::Cow;

use serde_json::{Value, json};

use crate::models::{AnchorSide, InlineCommentOut, ReviewResult, SubjectType};

/// Whether GitHub's `comments[]` cannot take this entry.
fn is_file_level(c: &InlineCommentOut) -> bool {
    c.subject_type == Some(SubjectType::File) || c.line == 0
}

/// The result as the review POST sends it (#10762).
///
/// Why: the demotion must not reach the `ReviewResult` JSON, which still
/// reports the file-level anchor in `inline_comments`.
/// What: borrows `result` when no entry is file-level. Otherwise returns a
/// copy without those entries in `inline_comments`, and without their
/// `finding_index` in `inline_finding_indices`, so the summary renderer
/// lists each demoted finding once, as it lists an off-diff finding.
/// Test: `file_level_and_line_zero_entries_demoted_to_summary_not_posted_inline`,
/// `demoted_entry_rendered_exactly_once_in_summary`.
pub(super) fn posting_view(result: &ReviewResult) -> Cow<'_, ReviewResult> {
    if !result.inline_comments.iter().any(is_file_level) {
        return Cow::Borrowed(result);
    }
    let mut view = result.clone();
    let (demoted, kept): (Vec<_>, Vec<_>) = view.inline_comments.drain(..).partition(is_file_level);
    view.inline_comments = kept;
    view.inline_finding_indices
        .retain(|i| !demoted.iter().any(|c| c.finding_index == *i));
    Cow::Owned(view)
}

/// The GitHub wire string of an anchor side.
fn side_str(side: AnchorSide) -> &'static str {
    match side {
        AnchorSide::Right => "RIGHT",
        AnchorSide::Left => "LEFT",
    }
}

/// One `comments[]` entry of the review POST (#10762).
///
/// What: `{path, line, side, body}` with the entry's own side; a range adds
/// `start_line` and `start_side`. The model holds no separate start side,
/// so `start_side` equals `side`.
/// Test: `post_payload_uses_left_side_and_start_line`.
pub(super) fn comment_json(c: &InlineCommentOut) -> Value {
    let side = side_str(c.side);
    let mut entry = json!({
        "path": c.path,
        "line": c.line,
        "side": side,
        "body": c.body,
    });
    if let (Some(start), Some(obj)) = (c.start_line, entry.as_object_mut()) {
        obj.insert("start_line".to_string(), json!(start));
        obj.insert("start_side".to_string(), json!(side));
    }
    entry
}
