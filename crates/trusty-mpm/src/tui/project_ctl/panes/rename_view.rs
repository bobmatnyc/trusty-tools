//! The session rename overlay (#10754).
//!
//! #10754: De1 key map (Bob 11-06) — `r` on a session row opens it.
//!
//! Why: the operator must see the name being typed and any refusal from
//! `validate_session_name` before the modal submits.
//! What: [`body_lines`] builds the overlay text from a
//! [`RenameModal`]; [`render`] floats it, centred, over the frame.
//! Test: `rename_view_lines_show_the_buffer_and_the_refusal`;
//! `r_opens_the_rename_modal_prefilled_and_enter_submits` draws it.

use crate::tui::theme::palette;
use ratatui::{
    Frame,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::config_form::centered_rect;
use crate::tui::project_ctl::state::rename::RenameModal;

/// Overlay width, in columns, including the border.
const OVERLAY_WIDTH: u16 = 60;
/// Overlay height, in rows, including the border.
const OVERLAY_HEIGHT: u16 = 7;

/// The overlay's body: the current name, the typed buffer with a cursor, the
/// refusal (if any) and the keys.
pub fn body_lines(modal: &RenameModal) -> Vec<String> {
    vec![
        format!("current: {}", modal.current_name),
        format!("new:     {}_", modal.typed),
        modal.error.clone().unwrap_or_default(),
        "[Enter] rename  [Esc] cancel".to_string(),
    ]
}

/// Draw the rename overlay over the whole frame.
pub fn render(frame: &mut Frame, modal: &RenameModal) {
    let area = centered_rect(OVERLAY_WIDTH, OVERLAY_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    let title = Line::from(vec![Span::styled(
        format!("Rename — {}", modal.current_name),
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
    fn rename_view_lines_show_the_buffer_and_the_refusal() {
        let mut modal = RenameModal::prefilled("id-1", "boss");
        let lines = body_lines(&modal);
        assert_eq!(lines[0], "current: boss");
        assert_eq!(lines[1], "new:     boss_");
        assert_eq!(lines[2], "");
        modal.error = Some("bad name".to_string());
        assert_eq!(body_lines(&modal)[2], "bad name");
    }
}
