//! Actions / key-hint bar (bottom row, 1 line) — DOC-35 §5 pane 4.
//!
//! Why: a single-line always-visible reference for the contextual verbs bound
//! to the focused pane, plus a live daemon-reachability indicator — mirrors
//! `tui::coordinator::layout`'s status bar.
//! What: [`bar_text`] is a pure builder (unit tested without a terminal);
//! [`render`] draws it reversed, matching every other TUI screen's status bar.
//! Test: `tests` covers the hint text and the notice/daemon-indicator
//! precedence.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

use crate::tui::project_ctl::state::ProjectCtlState;

/// The static key-hint text (DOC-35 §5 keybinding table, ASCII-mockup form —
/// see `events`'s module doc for why bare letters, not `j`/`k`, are the
/// action bindings).
pub const KEY_HINT: &str = "[l] launch  [k] kill  [r] resume  [d] decommission  [a] attach-cmd  [c] config   [Tab] switch pane  [↑↓] select  [Enter] drill in  [q] quit";

/// The indicator shown when the daemon answered its last health probe.
pub const DAEMON_REACHABLE: &str = "daemon ●";
/// The indicator shown when the daemon is unreachable.
pub const DAEMON_UNREACHABLE: &str = "daemon ✗";

/// Build the action bar's text for the current state.
///
/// Why: a transient notice (the result of a launch/kill/resume/decommission/
/// attach/config action) takes priority over the static key hints so the
/// operator sees the outcome of what they just did; either way the daemon
/// indicator stays visible on the right.
/// What: `"{notice or KEY_HINT}   ·   {daemon indicator}"`.
/// Test: `bar_text_shows_hint_by_default`, `bar_text_prefers_notice`,
/// `bar_text_shows_daemon_indicator`.
pub fn bar_text(state: &ProjectCtlState) -> String {
    let daemon = if state.daemon_reachable {
        DAEMON_REACHABLE
    } else {
        DAEMON_UNREACHABLE
    };
    let left = state.notice.as_deref().unwrap_or(KEY_HINT);
    format!("{left}   ·   {daemon}")
}

/// Draw the action bar into `area`.
///
/// Why: the single entry point [`super::super::layout::render`] calls for the
/// bottom row.
/// What: a single reversed/bold `Paragraph` line, matching every other TUI
/// screen's status-bar styling.
pub fn render(frame: &mut Frame, area: Rect, state: &ProjectCtlState) {
    let paragraph = Paragraph::new(Line::from(bar_text(state))).style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    );
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_text_shows_hint_by_default() {
        let state = ProjectCtlState::default();
        assert!(bar_text(&state).contains(KEY_HINT));
    }

    #[test]
    fn bar_text_prefers_notice() {
        let mut state = ProjectCtlState::default();
        state.set_notice("killed foo — now stopped");
        let text = bar_text(&state);
        assert!(text.contains("killed foo — now stopped"));
        assert!(!text.contains(KEY_HINT));
    }

    #[test]
    fn bar_text_shows_daemon_indicator() {
        let mut state = ProjectCtlState::default();
        assert!(bar_text(&state).contains(DAEMON_UNREACHABLE));
        state.daemon_reachable = true;
        assert!(bar_text(&state).contains(DAEMON_REACHABLE));
    }
}
