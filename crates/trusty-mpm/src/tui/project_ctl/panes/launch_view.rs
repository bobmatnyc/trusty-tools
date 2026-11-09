//! The launch form overlay `l` opens (#10754).
//!
//! See #10754: same centred, bordered style as the rename overlay.
//!
//! Why: the operator must see both fields, which one takes typed keys, and an
//! empty-task refusal before the form submits.
//! What: [`body_lines`] builds the overlay text from a [`LaunchModal`];
//! [`render`] floats it, centred, over the frame.
//! Test: `launch_view_lines_mark_the_focused_field_and_the_refusal`;
//! `l_opens_the_launch_form_and_enter_submits_the_request` draws it.

use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::config_form::centered_rect;
use crate::tui::project_ctl::state::launch::{LaunchField, LaunchModal};

/// Overlay width, in columns, including the border.
const OVERLAY_WIDTH: u16 = 76;
/// Overlay height, in rows, including the border.
const OVERLAY_HEIGHT: u16 = 7;

/// The overlay's body: the two fields (the focused one marked `>` and given
/// the cursor), the refusal (if any) and the keys.
pub fn body_lines(modal: &LaunchModal) -> Vec<String> {
    let field = |label: &str, value: &str, which: LaunchField| {
        let (mark, cursor) = if modal.field == which {
            (">", "_")
        } else {
            (" ", "")
        };
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
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(
        Paragraph::new(body_lines(modal).join("\n"))
            .style(Style::default().fg(Color::Reset))
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
}
