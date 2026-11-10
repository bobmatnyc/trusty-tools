//! The launch form overlay `l` opens (#10754).
//!
//! See #10754: same centred, bordered style as the rename overlay.
//!
//! Why: the operator must see both fields, which one takes typed keys, and an
//! empty-task refusal before the form submits.
//! What: [`body_lines`] builds the overlay text from a [`LaunchModal`];
//! [`render`] floats it, centred, over the frame.
//! Test: `launch_view_lines_mark_the_focused_field_and_the_refusal`,
//! `launch_view_long_task_shows_its_tail_and_the_cursor`;
//! `l_opens_the_launch_form_and_enter_submits_the_request` draws it.

use crate::tui::theme::palette;
use ratatui::{
    Frame,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::config_form::centered_rect;
use crate::tui::project_ctl::state::launch::{LaunchField, LaunchModal};

/// Overlay width, in columns, including the border.
const OVERLAY_WIDTH: u16 = 76;
/// Overlay height, in rows, including the border.
const OVERLAY_HEIGHT: u16 = 7;

/// Columns a field's value may fill: the inner width less the `> task: `
/// prefix and the `_` cursor.
const FIELD_WIDTH: usize = OVERLAY_WIDTH as usize - 2 - 8 - 1;

/// `value` if it fits in `width` display columns, else `…` and the value's
/// tail, together no wider than `width`.
///
/// See #10754: the cursor sits after the value, so the tail is what must stay
/// visible. Widths are display columns (`unicode-width`), not bytes.
fn fit_tail(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_string();
    }
    let budget = width.saturating_sub(1);
    let mut used = 0;
    let mut start = value.len();
    for (i, c) in value.char_indices().rev() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        start = i;
    }
    format!("…{}", &value[start..])
}

/// The overlay's body: the two fields (the focused one marked `>` and given
/// the cursor), the refusal (if any) and the keys. A value wider than
/// [`FIELD_WIDTH`] shows only its tail ([`fit_tail`]).
pub fn body_lines(modal: &LaunchModal) -> Vec<String> {
    let field = |label: &str, value: &str, which: LaunchField| {
        let (mark, cursor) = if modal.field == which {
            (">", "_")
        } else {
            (" ", "")
        };
        // See #10754: keep the cursor inside the overlay.
        let value = fit_tail(value, FIELD_WIDTH);
        format!("{mark} {label}{value}{cursor}")
    };
    vec![
        field("task: ", &modal.task, LaunchField::Task),
        field("ref:  ", &modal.git_ref, LaunchField::Ref),
        modal.error.clone().unwrap_or_default(),
        "[Enter] launch  [Tab] field  [Esc] cancel  (blank ref: default branch)".to_string(),
    ]
}

/// Draw the launch overlay over the whole frame.
pub fn render(frame: &mut Frame, modal: &LaunchModal) {
    let area = centered_rect(OVERLAY_WIDTH, OVERLAY_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    let title = Line::from(vec![Span::styled(
        format!("Launch — {}", modal.project),
        Style::default()
            .fg(palette().accent)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(
        Paragraph::new(body_lines(modal).join("\n"))
            .style(palette().overlay())
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_view_lines_mark_the_focused_field_and_the_refusal() {
        let mut modal = LaunchModal::open("alpha");
        modal.task = "fix it".to_string();
        let lines = body_lines(&modal);
        assert_eq!(lines[0], "> task: fix it_");
        assert_eq!(lines[1], "  ref:  ");
        assert_eq!(lines[2], "");
        modal.field = LaunchField::Ref;
        modal.error = Some("a task is required".to_string());
        let lines = body_lines(&modal);
        assert_eq!(lines[0], "  task: fix it");
        assert_eq!(lines[1], "> ref:  _");
        assert_eq!(lines[2], "a task is required");
    }

    /// See #10754: a value wider than the field shows `…` and its tail, so
    /// the line fits the overlay's 74 inner columns and keeps the cursor.
    #[test]
    fn launch_view_long_task_shows_its_tail_and_the_cursor() {
        let inner = usize::from(OVERLAY_WIDTH) - 2;
        // ASCII and double-width text: the cut is by display columns.
        for (task, tail) in [
            (
                format!("{}END", "x".repeat(90)),
                format!("{}END", "x".repeat(61)),
            ),
            ("漢".repeat(40), "漢".repeat(32)),
        ] {
            let mut modal = LaunchModal::open("alpha");
            modal.task = task;
            let line = body_lines(&modal).remove(0);
            assert_eq!(line, format!("> task: …{tail}_"));
            assert!(UnicodeWidthStr::width(line.as_str()) <= inner, "{line}");
        }
        // A value that fits renders unchanged.
        let mut modal = LaunchModal::open("alpha");
        modal.task = "y".repeat(FIELD_WIDTH);
        assert_eq!(body_lines(&modal)[0], format!("> task: {}_", modal.task));
    }
}
