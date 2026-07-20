//! The input composer row — generalized port of tagent's `chat.rs::draw_input`.
//!
//! Why: DOC-50 §5 Slice 4 migrates the borderless, Claude-Code-style prompt
//! row. Generalization dropped tagent's inline token counters (`↑{in}
//! ↓{out}`) — those are the same product-specific usage-tracking DOC-50 Q9
//! keeps out of the shared crate (no cost/usage formula here) — replacing
//! them with nothing on the right side while busy shows the cancel hint.
//! Everything else (prompt label, placeholder text swapping on busy/idle,
//! cursor positioning) is a direct, behavior-preserving port.
//!
//! What: [`draw_input`] renders `"{label}> {input_or_placeholder}"` and
//! positions the terminal cursor at the true edit position within the typed
//! text.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): migrate the input composer.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::ReplApp;

/// Placeholder shown while idle with an empty input buffer.
const IDLE_HINT: &str = "Ask anything, or /help for commands";

/// Placeholder shown while busy with an empty input buffer.
const BUSY_HINT: &str = "↑ to cancel";

/// Render the input composer row into `area` and position the terminal
/// cursor at the true edit position.
///
/// Why: a borderless single row (Claude Code style) — the `<label>> ` prompt
/// is sufficient demarcation without spending a row on a border.
/// What: empty input shows a dim italic placeholder (busy vs. idle hint);
/// non-empty input renders the buffer verbatim. The cursor is positioned by
/// counting chars up to `app.cursor_pos`, not bytes, so multi-byte input
/// doesn't desync the visual cursor from the true edit point.
/// Test: [`tests::draw_input_shows_idle_placeholder_when_empty`] and
/// friends exercise the pure text composition via [`compose_line`]; the
/// `Frame`/cursor-position side effects are integration-level and covered by
/// [`crate::layout::draw`]'s manual/visual verification.
pub fn draw_input(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let (line, cursor_col_offset) = compose_line(app);
    let p = Paragraph::new(line);
    f.render_widget(p, area);

    let cursor_col = area.x + cursor_col_offset as u16;
    let cursor_row = area.y;
    f.set_cursor_position((cursor_col, cursor_row));
}

/// Pure composition of the input row's `Line` plus the cursor's column
/// offset within it — split out from [`draw_input`] so the text/placeholder
/// logic is unit-testable without a `Frame`.
fn compose_line(app: &ReplApp) -> (Line<'static>, usize) {
    let prompt = format!("{}> ", app.label);
    let prompt_width = prompt.chars().count();

    let mut spans: Vec<Span<'static>> = vec![Span::raw(prompt.clone())];
    if app.input_buf.is_empty() {
        let placeholder = if app.busy { BUSY_HINT } else { IDLE_HINT };
        spans.push(Span::styled(
            placeholder.to_string(),
            Style::default()
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        spans.push(Span::raw(app.input_buf.clone()));
    }

    let cursor_col = prompt_width + app.input_buf[..app.cursor_pos].chars().count();
    (Line::from(spans), cursor_col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ReplApp;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn draw_input_shows_idle_placeholder_when_empty() {
        let app = ReplApp::new("demo", "u");
        let (line, cursor) = compose_line(&app);
        assert!(line_text(&line).contains(IDLE_HINT));
        assert_eq!(cursor, "demo> ".chars().count());
    }

    #[test]
    fn draw_input_shows_busy_placeholder_when_empty_and_busy() {
        let mut app = ReplApp::new("demo", "u");
        app.busy = true;
        let (line, _) = compose_line(&app);
        assert!(line_text(&line).contains(BUSY_HINT));
    }

    #[test]
    fn draw_input_renders_typed_buffer_verbatim() {
        let mut app = ReplApp::new("demo", "u");
        app.set_input("hello".to_string());
        let (line, cursor) = compose_line(&app);
        assert!(line_text(&line).contains("hello"));
        assert_eq!(cursor, "demo> hello".chars().count());
    }

    #[test]
    fn draw_input_cursor_offset_respects_multibyte_chars() {
        let mut app = ReplApp::new("demo", "u");
        app.set_input("héllo".to_string());
        app.cursor_pos = "h".len() + "é".len(); // after the é (byte offset)
        let (_, cursor) = compose_line(&app);
        // "demo> " (6 chars) + "h" + "é" = 8 chars, not a byte count.
        assert_eq!(cursor, 8);
    }
}
