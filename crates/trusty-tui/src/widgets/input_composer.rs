//! The input composer row — generalized port of tagent's `chat.rs::draw_input`.
//!
//! Why: DOC-50 §5 Slice 4 migrates the borderless, Claude-Code-style prompt
//! row. Generalization dropped tagent's inline token counters (`↑{in}
//! ↓{out}`) — the same product-specific usage-tracking DOC-50 Q9 keeps out
//! of the shared crate (no cost/usage formula here). Everything else —
//! prompt label, the empty-buffer placeholder swapping busy/idle text, AND
//! the right-aligned `[thinking...]` decoration — is a direct,
//! width-aware port: tagent renders that decoration whenever `app.thinking`
//! is true, REGARDLESS of whether the input buffer is empty (a user typing
//! ahead while the agent is still responding still sees it). An earlier
//! revision of this module only surfaced busy state through the empty-
//! buffer placeholder, which silently dropped that case — see
//! `crate::app`'s module doc comment for the full disclosure list.
//!
//! What: [`draw_input`] renders `"{label}> {input_or_placeholder}"` with the
//! `[thinking...]` label right-aligned when `app.busy` and space allows, and
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

/// Right-aligned decoration shown whenever `app.busy`, independent of
/// buffer contents — matches tagent's `thinking_label`.
const BUSY_LABEL: &str = "[thinking...]";

/// Render the input composer row into `area` and position the terminal
/// cursor at the true edit position.
///
/// Why: a borderless single row (Claude Code style) — the `<label>> ` prompt
/// is sufficient demarcation without spending a row on a border.
/// What: empty input shows a dim italic placeholder (busy vs. idle hint);
/// non-empty input renders the buffer verbatim; either way, a busy state
/// additionally right-aligns `[thinking...]` if the row is wide enough.
/// The cursor is positioned by counting chars up to `app.cursor_pos`, not
/// bytes, so multi-byte input doesn't desync the visual cursor from the true
/// edit point.
/// Test: [`tests::draw_input_shows_idle_placeholder_when_empty`] and
/// friends exercise the pure text composition via [`compose_line`]; the
/// `Frame`/cursor-position side effects are integration-level and covered by
/// [`crate::layout::draw`]'s manual/visual verification.
pub fn draw_input(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let (line, cursor_col_offset) = compose_line(app, area.width as usize);
    let p = Paragraph::new(line);
    f.render_widget(p, area);

    let cursor_col = area.x + cursor_col_offset as u16;
    let cursor_row = area.y;
    f.set_cursor_position((cursor_col, cursor_row));
}

/// Pure composition of the input row's `Line` plus the cursor's column
/// offset within it — split out from [`draw_input`] so the text/placeholder/
/// busy-label logic is unit-testable without a `Frame`.
///
/// Why: `width` is needed (not just `app`) to replicate tagent's exact
/// right-alignment math for `[thinking...]` — `crate::layout::draw` reserves
/// a fixed 1-row height for this widget, so unlike the scrollback pane there
/// is no pre-layout content-height query; `width` alone is what the padding
/// calculation needs.
fn compose_line(app: &ReplApp, width: usize) -> (Line<'static>, usize) {
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

    // Right-aligned busy decoration — independent of whether the buffer is
    // empty (matches tagent's `chat.rs::draw_input`: `used` is computed from
    // `input_buf`'s real length, not the placeholder text, so both the
    // empty-buffer placeholder AND this label can render together).
    if app.busy {
        let used = prompt_width + app.input_buf.chars().count();
        let label_w = BUSY_LABEL.chars().count();
        if width > used + label_w + 1 {
            let pad = width - used - label_w;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                BUSY_LABEL,
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }

    (Line::from(spans), cursor_col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ReplApp;

    /// Wide enough that the busy label's padding math never truncates it
    /// away in tests that aren't specifically exercising the narrow-width
    /// case.
    const WIDE: usize = 120;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn draw_input_shows_idle_placeholder_when_empty() {
        let app = ReplApp::new("demo", "u");
        let (line, cursor) = compose_line(&app, WIDE);
        assert!(line_text(&line).contains(IDLE_HINT));
        assert_eq!(cursor, "demo> ".chars().count());
    }

    #[test]
    fn draw_input_shows_busy_placeholder_when_empty_and_busy() {
        let mut app = ReplApp::new("demo", "u");
        app.busy = true;
        let (line, _) = compose_line(&app, WIDE);
        assert!(line_text(&line).contains(BUSY_HINT));
    }

    #[test]
    fn draw_input_renders_typed_buffer_verbatim() {
        let mut app = ReplApp::new("demo", "u");
        app.set_input("hello".to_string());
        let (line, cursor) = compose_line(&app, WIDE);
        assert!(line_text(&line).contains("hello"));
        assert_eq!(cursor, "demo> hello".chars().count());
    }

    #[test]
    fn draw_input_cursor_offset_respects_multibyte_chars() {
        let mut app = ReplApp::new("demo", "u");
        app.set_input("héllo".to_string());
        app.cursor_pos = "h".len() + "é".len(); // after the é (byte offset)
        let (_, cursor) = compose_line(&app, WIDE);
        // "demo> " (6 chars) + "h" + "é" = 8 chars, not a byte count.
        assert_eq!(cursor, 8);
    }

    /// The HIGH-severity regression this module exists to fix: a user
    /// typing ahead (non-empty buffer) while the agent is still responding
    /// must still see a busy indicator — not just when the buffer happens
    /// to be empty.
    #[test]
    fn draw_input_shows_busy_label_while_typing_with_nonempty_buffer() {
        let mut app = ReplApp::new("demo", "u");
        app.busy = true;
        app.set_input("still typing".to_string());
        let (line, _) = compose_line(&app, WIDE);
        let text = line_text(&line);
        assert!(
            text.contains("still typing"),
            "typed text must survive: {text}"
        );
        assert!(
            text.contains(BUSY_LABEL),
            "busy label must render even with a non-empty buffer: {text}"
        );
    }

    #[test]
    fn draw_input_omits_busy_label_when_idle() {
        let mut app = ReplApp::new("demo", "u");
        app.set_input("hello".to_string());
        app.busy = false;
        let (line, _) = compose_line(&app, WIDE);
        assert!(!line_text(&line).contains(BUSY_LABEL));
    }

    /// When the row is too narrow to fit the label without colliding with
    /// the prompt/typed text, it must be omitted rather than overlapping —
    /// matches tagent's `total_width > used + label_w + 1` guard.
    #[test]
    fn draw_input_omits_busy_label_when_too_narrow() {
        let mut app = ReplApp::new("demo", "u");
        app.busy = true;
        app.set_input("hello".to_string());
        let (line, _) = compose_line(&app, 10);
        assert!(!line_text(&line).contains(BUSY_LABEL));
    }
}
