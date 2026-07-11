//! Actions / key-hint bar (bottom row, 1 line) — DOC-35 §5 pane 4.
//!
//! Why: a single-line always-visible reference for the contextual verbs bound
//! to the focused pane, plus a live daemon-reachability indicator — mirrors
//! `tui::coordinator::layout`'s status bar. DOC-35 §5.1 ("contextual verbs
//! bound to the focused pane") and issue #2118 both require the hint text to
//! change with [`Pane`] focus, not read as one fixed string: showing
//! `[k] kill  [d] decommission  …` while the Projects or Activity pane is
//! focused would be misleading — those keys are no-ops there.
//! What: [`focus_hint`] / [`confirm_prompt_text`] / [`bar_text`] are pure
//! builders (unit tested without a terminal); [`render`] draws the result
//! reversed, matching every other TUI screen's status bar.
//! Test: `tests` covers each focus's hint text, notice/confirm precedence
//! over the hint, and the daemon indicator.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

use crate::tui::project_ctl::state::{Pane, PendingConfirm, ProjectCtlState};

/// Key-hint text shown while the Projects pane has focus.
pub const PROJECTS_HINT: &str =
    "[Enter] drill in  [c] config  [Tab] switch pane  [↑↓] select  [q] quit";
/// Key-hint text shown while the Sessions pane has focus.
pub const SESSIONS_HINT: &str = "[l] launch  [k] kill  [r] resume  [d] decommission  [a] attach-cmd  [Tab] switch pane  [↑↓] select  [q] quit";
/// Key-hint text shown while the Activity pane has focus (no verbs bound
/// there — it is a read-only status strip).
pub const ACTIVITY_HINT: &str = "[Tab] switch pane  [q] quit";

/// The indicator shown when the daemon answered its last health probe.
pub const DAEMON_REACHABLE: &str = "daemon ●";
/// The indicator shown when the daemon is unreachable.
pub const DAEMON_UNREACHABLE: &str = "daemon ✗";

/// Pick the key-hint text for a given pane focus (DOC-35 §5.1).
///
/// Why: pulled out of [`bar_text`] so the per-focus mapping is directly
/// unit-testable without going through the notice/confirm precedence chain.
/// What: [`PROJECTS_HINT`] / [`SESSIONS_HINT`] / [`ACTIVITY_HINT`] per
/// [`Pane`] variant.
/// Test: `focus_hint_differs_per_pane`.
pub fn focus_hint(focus: Pane) -> &'static str {
    match focus {
        Pane::Projects => PROJECTS_HINT,
        Pane::Sessions => SESSIONS_HINT,
        Pane::Activity => ACTIVITY_HINT,
    }
}

/// Build the confirmation-gate prompt text for a [`PendingConfirm`] (DOC-35 §5.2).
///
/// Why: a pure builder keeps the exact prompt wording testable without a
/// terminal; `kill` and `decommission` get distinct wording since only the
/// latter is described as permanent.
/// What: names the verb, the target session's label, the consequence, and the
/// `y`/`n` keys that resolve it.
/// Test: `confirm_prompt_text_names_verb_and_target`.
pub fn confirm_prompt_text(confirm: &PendingConfirm) -> String {
    use crate::tui::project_ctl::state::ConfirmKind;
    match confirm.kind {
        ConfirmKind::Kill => format!(
            "⚠ kill '{}' — session is Active. Confirm? [y] yes  [n/Esc] cancel",
            confirm.session_label
        ),
        ConfirmKind::Decommission => format!(
            "⚠ decommission '{}' — PERMANENT, deletes the workspace. Confirm? [y] yes  [n/Esc] cancel",
            confirm.session_label
        ),
    }
}

/// Build the action bar's text for the current state.
///
/// Why: three things can occupy the left side of the bar, in strict priority
/// order — an open confirmation gate (DOC-35 §5.2) is effectively a modal and
/// must dominate everything else; failing that, a transient notice (the
/// result of the operator's last action) should be seen before it is
/// overwritten by the next poll; failing that, the focus-contextual hint
/// (DOC-35 §5.1). The daemon indicator stays visible on the right regardless.
/// What: `"{confirm-prompt or notice or focus_hint}   ·   {daemon indicator}"`.
/// Test: `bar_text_shows_focus_hint_by_default`,
/// `bar_text_prefers_notice_over_hint`,
/// `bar_text_prefers_confirm_over_notice`,
/// `bar_text_shows_daemon_indicator`.
pub fn bar_text(state: &ProjectCtlState) -> String {
    let daemon = if state.daemon_reachable {
        DAEMON_REACHABLE
    } else {
        DAEMON_UNREACHABLE
    };
    let left = if let Some(confirm) = &state.pending_confirm {
        confirm_prompt_text(confirm)
    } else if let Some(notice) = &state.notice {
        notice.clone()
    } else {
        focus_hint(state.focus).to_string()
    };
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
    use crate::tui::project_ctl::state::ConfirmKind;

    #[test]
    fn focus_hint_differs_per_pane() {
        let projects = focus_hint(Pane::Projects);
        let sessions = focus_hint(Pane::Sessions);
        let activity = focus_hint(Pane::Activity);
        assert_ne!(projects, sessions);
        assert_ne!(sessions, activity);
        assert_ne!(projects, activity);

        // Destructive/session verbs only appear in the Sessions hint.
        assert!(sessions.contains("[k] kill"));
        assert!(sessions.contains("[d] decommission"));
        assert!(!projects.contains("[k] kill"));
        assert!(!activity.contains("[k] kill"));

        // The Projects-only config verb only appears there.
        assert!(projects.contains("[c] config"));
        assert!(!sessions.contains("[c] config"));
    }

    #[test]
    fn bar_text_shows_focus_hint_by_default() {
        let mut state = ProjectCtlState::default();
        assert_eq!(state.focus, Pane::Projects);
        assert!(bar_text(&state).contains(PROJECTS_HINT));

        state.focus = Pane::Sessions;
        assert!(bar_text(&state).contains(SESSIONS_HINT));

        state.focus = Pane::Activity;
        assert!(bar_text(&state).contains(ACTIVITY_HINT));
    }

    #[test]
    fn bar_text_prefers_notice_over_hint() {
        let mut state = ProjectCtlState::default();
        state.set_notice("killed foo — now stopped");
        let text = bar_text(&state);
        assert!(text.contains("killed foo — now stopped"));
        assert!(!text.contains(PROJECTS_HINT));
    }

    #[test]
    fn bar_text_prefers_confirm_over_notice() {
        let mut state = ProjectCtlState::default();
        state.set_notice("this should be hidden");
        state.request_confirm(ConfirmKind::Decommission, "sess-1", "my-session");
        let text = bar_text(&state);
        assert!(text.contains("decommission"));
        assert!(text.contains("my-session"));
        assert!(!text.contains("this should be hidden"));
    }

    #[test]
    fn confirm_prompt_text_names_verb_and_target() {
        let kill = PendingConfirm {
            kind: ConfirmKind::Kill,
            session_id: "id".to_string(),
            session_label: "my-session".to_string(),
        };
        let kill_text = confirm_prompt_text(&kill);
        assert!(kill_text.contains("kill"));
        assert!(kill_text.contains("my-session"));
        assert!(kill_text.contains("[y] yes"));
        assert!(kill_text.contains("[n/Esc] cancel"));

        let decommission = PendingConfirm {
            kind: ConfirmKind::Decommission,
            session_id: "id".to_string(),
            session_label: "my-session".to_string(),
        };
        let decommission_text = confirm_prompt_text(&decommission);
        assert!(decommission_text.contains("decommission"));
        assert!(decommission_text.contains("PERMANENT"));
    }

    #[test]
    fn bar_text_shows_daemon_indicator() {
        let mut state = ProjectCtlState::default();
        assert!(bar_text(&state).contains(DAEMON_UNREACHABLE));
        state.daemon_reachable = true;
        assert!(bar_text(&state).contains(DAEMON_REACHABLE));
    }
}
