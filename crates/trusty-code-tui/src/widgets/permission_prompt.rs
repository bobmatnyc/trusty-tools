//! The modal permission prompt (#3422).
//!
//! Why: a tool call suspended by an `ask` rule is invisible without it — the
//! backend simply stops, and the TUI looks hung until the request times out
//! and is denied. This widget is what turns that stall into a question, and
//! it is the only place the three answers are spelled for the user.
//! What: [`permission_prompt_lines`] turns one
//! [`PendingPermission`](crate::model::PendingPermission) into the rows
//! [`draw_permission_prompt`] renders above the input composer;
//! [`crate::layout::draw`] reserves the rows only while a prompt is open.
//! Nothing here decides anything — the key bindings it advertises are the
//! ones `crate::app::reduce` binds, and the decision itself belongs to the
//! backend (ADR-0063).
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 9 deliverable (§5, Slice 9): permission prompts.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::ReplApp;
use crate::model::PendingPermission;

/// The answer keys, in the order the footer lists them. Kept beside the
/// widget so the advertised bindings and `reduce::permission_answer_for_key`
/// can be compared in one test rather than drifting silently.
const ANSWER_HINTS: &[(&str, &str)] = &[
    ("y", "allow once"),
    ("a", "allow for session"),
    ("n/Esc", "deny"),
];

/// How many rows a prompt occupies: header, subject, rule, answers.
pub const PROMPT_HEIGHT: u16 = 4;

/// The rows [`crate::layout::draw`] must reserve for `app`'s prompt.
///
/// Why: the prompt's height is otherwise fixed by design (see
/// [`permission_prompt_lines`]), but a failed answer adds one retry row —
/// and a layout that reserved [`PROMPT_HEIGHT`] regardless would silently
/// clip exactly the line explaining why the prompt came back (#3422).
/// What: `0` with no prompt open, [`PROMPT_HEIGHT`] normally, one more when
/// [`ReplApp::permission_error`] is set.
/// Test: `tests::prompt_height_grows_by_one_row_for_a_retry_line`.
pub fn prompt_height(app: &ReplApp) -> u16 {
    if app.pending_permission.is_none() {
        return 0;
    }
    PROMPT_HEIGHT + u16::from(app.permission_error.is_some())
}

/// Render the open prompt into `area`.
///
/// Why: a plain `Paragraph` rather than a bordered overlay — the scrollback
/// and input row are borderless too, and a floating box would cost two rows
/// of an already tight layout to say nothing extra.
/// What: draws [`permission_prompt_lines`] for `app.pending_permission`, or
/// nothing at all when no prompt is open.
/// Test: `crate::layout::tests::draw_renders_without_panicking` exercises
/// the `Frame` path; the text itself is covered by the pure-function tests
/// below.
pub fn draw_permission_prompt(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let Some(pending) = app.pending_permission.as_ref() else {
        return;
    };
    let lines = permission_prompt_lines(pending, app.permission_error.as_deref());
    f.render_widget(Paragraph::new(lines), area);
}

/// The prompt's rows: what is being asked, on what, under which rule, and
/// which key answers how.
///
/// Why: split from [`draw_permission_prompt`] so the wording is unit-testable
/// without a `Frame`, matching [`crate::widgets::tool_card::tool_card_lines`].
/// What: exactly [`PROMPT_HEIGHT`] rows, plus one leading retry row when
/// `error` is `Some` (#3422 — the answer that never reached the backend; see
/// [`crate::event::ReplEvent::PermissionAnswerFailed`]). The subject row
/// renders the producer's already-redacted text verbatim, folded onto one
/// line so a multi-path subject cannot push the input composer off screen;
/// an empty subject (a tool that acts on nothing nameable) still gets its
/// row, so the prompt's height never changes under the REQUEST — only under
/// a failure the operator must see.
/// Test: `tests::prompt_names_the_tool_subject_and_rule`,
/// `tests::prompt_with_an_empty_subject_keeps_its_height`,
/// `tests::prompt_folds_a_multi_line_subject_onto_one_row`,
/// `tests::prompt_shows_the_retry_line_after_a_failed_answer`.
pub fn permission_prompt_lines(
    pending: &PendingPermission,
    error: Option<&str>,
) -> Vec<Line<'static>> {
    let warn = Style::default().fg(Color::Yellow);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);

    let header = Line::from(vec![
        Span::styled("⏸ permission ", warn.add_modifier(Modifier::BOLD)),
        Span::styled(format!("{} wants to run ", pending.agent), dim),
        Span::styled(pending.tool.clone(), bold),
    ]);
    let subject = Line::from(vec![
        Span::raw("  "),
        Span::styled(fold_to_one_line(&pending.subject), Style::default()),
    ]);
    let rule = Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("matched rule: {}", pending.rule), dim),
    ]);

    let mut answers = vec![Span::raw("  ")];
    for (i, (key, label)) in ANSWER_HINTS.iter().enumerate() {
        if i > 0 {
            answers.push(Span::styled("   ", dim));
        }
        answers.push(Span::styled(format!("[{key}]"), warn));
        answers.push(Span::styled(format!(" {label}"), dim));
    }

    let mut lines = Vec::with_capacity(PROMPT_HEIGHT as usize + 1);
    // #3422: the retry row leads, above the header — the operator has to
    // learn their previous answer was LOST before re-reading the question,
    // or a reopened prompt just looks like the backend asked twice.
    if let Some(error) = error {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                fold_to_one_line(error),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    lines.extend([header, subject, rule, Line::from(answers)]);
    lines
}

/// Collapse `text` onto one row, joining its non-blank lines with `" · "`.
///
/// Why: a subject can legitimately be several file paths separated by
/// newlines; rendering them as rows would make the prompt's height depend on
/// the request and could push the input composer off a short terminal.
fn fold_to_one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests;
