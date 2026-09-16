//! The top-level frame composition — generalized port of tagent's
//! `repl/tui/layout.rs::draw`.
//!
//! Why: DOC-50 §5 Slice 4 migrates the vertical split that arranges the
//! scrollback, input row, and statusline into one frame. Generalization
//! dropped two tagent-only rows this crate has no generic equivalent for
//! yet: the three-row "activity panel" (animated spinner + elapsed timer +
//! latest thinking step, `repl/tui/layout.rs::draw_activity` /
//! `status.rs`'s spinner helpers) and the inline choice-picker row (model/
//! provider/persona pickers, `repl/tui/pickers.rs`) — both are Phase 2/
//! later-slice concerns per DOC-50 §4.2/§5 Slice 7-9, not part of the MVP
//! scrollback+input+statusline widget set this slice delivers.
//!
//! What: [`draw`] splits the frame into chat (sized to content height, per
//! [`crate::widgets::scrollback::chat_line_count`]), a separator, the input
//! row, another separator, and the statusline, with a trailing blank spacer.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): migrate `layout.rs`.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;

use crate::app::ReplApp;
use crate::widgets::banner::banner_lines;
use crate::widgets::input_composer::draw_input;
use crate::widgets::permission_prompt::{draw_permission_prompt, prompt_height};
use crate::widgets::scrollback::{chat_line_count, draw_chat};
use crate::widgets::status_line::draw_statusline;

/// Render a dim horizontal rule across `area` — the visual divider above and
/// below the input row.
///
/// Why: a single `─`-repeated row gives the borderless input a clear
/// top/bottom demarcation without the vertical cost of a bordered `Block`.
pub fn draw_separator(f: &mut ratatui::Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let rule: String = "─".repeat(area.width as usize);
    let p = Paragraph::new(rule).style(Style::default().add_modifier(Modifier::DIM));
    f.render_widget(p, area);
}

/// Top-level draw: banner (in-scrollback) + chat (sized to content) +
/// separator + input + separator + statusline + trailing spacer.
///
/// Why: sizing the chat pane to its actual content height (rather than
/// `Constraint::Min`, which expands to fill the screen) lets the input row
/// float up directly beneath the last message instead of always being
/// pinned to the bottom of the terminal — matches tagent's `layout.rs`
/// rationale (issue #337 upstream).
/// What: reserves fixed 1-row constraints for both separators, the input
/// row, and the statusline, then gives the chat pane `min(content_height,
/// available_height)`. A trailing `Constraint::Min(1)` spacer absorbs
/// leftover space so nothing gets stretched to fill the terminal.
/// Test: pure geometry is covered by
/// [`crate::widgets::scrollback::chat_line_count`]'s own tests; the full
/// `Frame` composition is exercised via `ratatui::backend::TestBackend` in
/// `tests::draw_renders_without_panicking`, which is a smoke test (no
/// TTY, no assertions on cell contents — see that test's doc comment).
pub fn draw(f: &mut ratatui::Frame, app: &ReplApp) {
    let area = f.area();

    let content_h = if app.chat.is_empty() && !app.show_banner {
        0
    } else if app.chat.is_empty() && app.show_banner {
        banner_lines(app, area.width as usize).len()
    } else {
        chat_line_count(app, area.width as usize).max(1)
    };

    // #3422: the prompt sits directly above the input row, and costs its rows
    // only while a request is actually pending. Given to the chat pane
    // otherwise, so an idle frame looks exactly as it did before. The height
    // is asked for rather than assumed, because a reopened prompt carries one
    // extra retry row — see `prompt_height`.
    let prompt_h = prompt_height(app);

    // Reserved rows below chat: prompt + top_sep(1) + input(1) + bot_sep(1) +
    // statusline(1) + bottom spacer minimum(1).
    let reserved: u16 = 5 + prompt_h;
    let available_h = area.height.saturating_sub(reserved) as usize;
    let chat_h = content_h
        .min(available_h)
        .max(if content_h == 0 { 0 } else { 1 });

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(chat_h as u16),
            Constraint::Length(prompt_h), // permission prompt (0 when none)
            Constraint::Length(1),        // separator above input
            Constraint::Length(1),        // input
            Constraint::Length(1),        // separator below input
            Constraint::Length(1),        // statusline
            Constraint::Min(1),           // trailing spacer
        ])
        .split(area);

    draw_chat(f, app, chunks[0]);
    draw_permission_prompt(f, app, chunks[1]);
    draw_separator(f, chunks[2]);
    draw_input(f, app, chunks[3]);
    draw_separator(f, chunks[4]);
    draw_statusline(f, app, chunks[5]);
    f.render_widget(Paragraph::new(""), chunks[6]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Smoke test: `draw` must not panic across the shapes that matter most
    /// — empty app (banner only), populated chat, and a tiny terminal where
    /// every `Constraint::Length` competes for the same few rows. This does
    /// NOT assert on cell contents (that's the pure-function tests' job in
    /// each widget module); it exists purely to catch a layout arithmetic
    /// panic (e.g. an unchecked subtraction) before it reaches a real TTY.
    #[test]
    fn draw_renders_without_panicking() {
        for (w, h) in [(80u16, 24u16), (120, 40), (10, 3)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).expect("construct terminal");
            let mut app = ReplApp::new("demo", "bob");
            terminal.draw(|f| draw(f, &app)).expect("draw empty app");

            app.push_user("hello");
            app.push_assistant("hi there", false);
            terminal
                .draw(|f| draw(f, &app))
                .expect("draw populated app");

            // #3422: the prompt steals four rows, so the tiny-terminal case
            // above is where an unchecked subtraction would surface.
            app.pending_permission = Some(crate::model::PendingPermission {
                request_id: "req-1".to_string(),
                agent: "python-engineer".to_string(),
                agent_id: "spawn-1".to_string(),
                tool: "bash".to_string(),
                subject: "rm -rf build".to_string(),
                rule: "bash[rm *]".to_string(),
            });
            terminal
                .draw(|f| draw(f, &app))
                .expect("draw app with a pending permission prompt");
        }
    }
}
