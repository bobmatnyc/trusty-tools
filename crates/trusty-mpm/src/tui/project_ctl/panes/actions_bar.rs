//! Actions / key-hint bar (bottom row, 1 line) — DOC-35 §5 pane 4.
//!
//! Why: a single-line always-visible reference for the contextual verbs bound
//! to the focused pane, plus a live daemon-reachability indicator — mirrors
//! `tui::coordinator::layout`'s status bar. DOC-35 §5.1 ("contextual verbs
//! bound to the focused pane") and issue #2118 both require the hint text to
//! change with [`Pane`] focus, not read as one fixed string: showing
//! `[k] kill  [d] decommission  …` while the Projects or Activity pane is
//! focused would be misleading — those keys are no-ops there.
//! #10476: in the tree, the hint also follows the selected node's kind.
//! #10754: two-column MPM pane (Bob 11-06) — the session column has its own
//! hint, which carries the session verbs and, since the details overlay
//! went, the expand/collapse keys.
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

use crate::tui::project_ctl::state::{NodeId, Pane, PendingConfirm, ProjectCtlState};

/// Key-hint text while an owner bar is selected in the left column.
pub const OWNER_HINT: &str =
    "[Enter/←→] expand  [↑↓] select  [g] workers  [Tab] switch pane  [q] quit";
/// Key-hint text while a project is selected in the left column.
// #10754: De1 key map (Bob 11-06); `l` is `n` until a launch form exists.
pub const PROJECTS_HINT: &str = "[→/Enter] sessions  [n/l] new  [c] config  [v] deliverables  [g] workers  [Tab] switch pane  [q] quit";
/// Key-hint text while the session column has focus.
// #10754: De1 key map (Bob 11-06)
pub const SESSIONS_HINT: &str = "[Enter] open  [→] details  [←] collapse/projects  [f] focus  [r] rename  [R] resume  [n/l] new  [k] kill  [d] decommission  [a] attach-cmd  [q] quit";
/// Key-hint text shown while the Activity pane has focus. It is a read-only
/// status strip; #10754: `n`/`l` start a new session from it, as elsewhere.
pub const ACTIVITY_HINT: &str = "[n/l] new  [Tab] switch pane  [q] quit";

/// The indicator shown when the daemon answered its last health probe.
pub const DAEMON_REACHABLE: &str = "daemon ●";
/// The indicator shown when the daemon is unreachable.
pub const DAEMON_UNREACHABLE: &str = "daemon ✗";

/// Pick the key-hint text for the focused pane and selected node (DOC-35 §5.1).
///
/// Why: pulled out of [`bar_text`] so the mapping is directly unit-testable
/// without going through the notice/confirm precedence chain.
/// What: [`ACTIVITY_HINT`] for the Activity pane; [`SESSIONS_HINT`] for the
/// session column (#10754); in the tree, [`OWNER_HINT`] / [`PROJECTS_HINT`]
/// by the selected node (nothing selected reads as an owner).
/// Test: `focus_hint_differs_per_node_kind`.
pub fn focus_hint(state: &ProjectCtlState) -> &'static str {
    if state.focus == Pane::Activity {
        return ACTIVITY_HINT;
    }
    // #10754: two-column MPM pane (Bob 11-06)
    if state.focus == Pane::Sessions {
        return SESSIONS_HINT;
    }
    match state.selected_node() {
        Some(NodeId::Project { .. }) => PROJECTS_HINT,
        Some(NodeId::Owner(_)) | None => OWNER_HINT,
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
        focus_hint(state).to_string()
    };
    format!("{left}   ·   {daemon}")
}

/// The label a click treats as the Focus button (#10476).
const FOCUS_LABEL: &str = "[f] focus";

/// Where `area` shows the Focus button, if this frame's bar shows one.
///
/// Why: the owner asked for a Focus key or button (#10476 comment 2); the hint
/// text already names the key, so that label is the button.
/// What: the label's cells within `area` while the bar shows the session
/// hint. A confirm prompt or a notice replaces the hint, and with it the
/// button.
/// Test: `focus_button_tracks_the_visible_label`.
pub fn focus_button(state: &ProjectCtlState, area: Rect) -> Option<Rect> {
    if state.pending_confirm.is_some() || state.notice.is_some() {
        return None;
    }
    let hint = focus_hint(state);
    if hint != SESSIONS_HINT {
        return None;
    }
    let start = hint.find(FOCUS_LABEL)?;
    let column = u16::try_from(hint[..start].chars().count()).ok()?;
    let label = Rect {
        x: area.x.saturating_add(column),
        y: area.y,
        width: FOCUS_LABEL.len() as u16,
        height: 1,
    }
    .intersection(area);
    (!label.is_empty()).then_some(label)
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

    /// acme/alpha with one session, selected at `node` (a session id selects
    /// it in the right column and focuses that column).
    fn state_at(node: Option<Node>) -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![crate::tui::project_ctl::state::ProjectRow {
                name: "alpha".to_string(),
                repo_url: "https://github.com/acme/alpha".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        state.sessions_by_project.insert(
            "alpha".to_string(),
            vec![crate::tui::project_ctl::state::SessionRow {
                id: "s1".to_string(),
                ..Default::default()
            }],
        );
        match node {
            Some(Node::Tree(node)) => state.select_node(node),
            Some(Node::Session(id)) => assert!(state.focus_session(id)),
            None => state.rebuild_tree(),
        }
        state
    }

    /// Where a test state's selection sits.
    enum Node {
        Tree(NodeId),
        Session(&'static str),
    }

    fn project_node() -> Node {
        Node::Tree(NodeId::Project {
            owner: "acme".to_string(),
            name: "alpha".to_string(),
        })
    }

    fn session_node() -> Node {
        Node::Session("s1")
    }

    #[test]
    fn focus_hint_differs_per_node_kind() {
        let owner = focus_hint(&state_at(None));
        let projects = focus_hint(&state_at(Some(project_node())));
        let sessions = focus_hint(&state_at(Some(session_node())));
        let mut activity_state = state_at(None);
        activity_state.focus = Pane::Activity;
        let activity = focus_hint(&activity_state);
        assert_eq!(owner, OWNER_HINT);
        assert_ne!(projects, sessions);
        assert_ne!(sessions, activity);
        assert_ne!(projects, activity);

        // Destructive/session verbs only appear in the Sessions hint.
        assert!(sessions.contains("[k] kill"));
        assert!(sessions.contains("[d] decommission"));
        assert!(!projects.contains("[k] kill"));
        assert!(!activity.contains("[k] kill"));

        assert!(!owner.contains("[k] kill"));

        // The Projects-only config verb only appears there.
        assert!(projects.contains("[c] config"));
        assert!(!sessions.contains("[c] config"));

        // #10754: De1 key map (Bob 11-06)
        for keys in [
            "[Enter] open",
            "[→] details",
            "[←] collapse/projects",
            "[r] rename",
            "[R] resume",
            "[n/l] new",
        ] {
            assert!(sessions.contains(keys), "{keys}");
        }
        assert!(projects.contains("[n/l] new"));
        assert!(activity.contains("[n/l] new"));
        assert!(sessions.contains("[a] attach-cmd"));
        let mut expanded = state_at(Some(session_node()));
        assert!(expanded.toggle_session_details());
        assert_eq!(focus_hint(&expanded), SESSIONS_HINT);
    }

    #[test]
    fn focus_button_tracks_the_visible_label() {
        let area = Rect::new(2, 9, 100, 1);
        let mut state = state_at(Some(session_node()));
        let button = focus_button(&state, area).expect("a session shows the button");
        let byte = SESSIONS_HINT.find(FOCUS_LABEL).expect("the hint names it");
        // #10754: the hint holds multi-byte arrows; a cell is a char.
        let start = SESSIONS_HINT[..byte].chars().count() as u16;
        assert_eq!(button, Rect::new(2 + start, 9, FOCUS_LABEL.len() as u16, 1));
        let drawn: String = bar_text(&state)
            .chars()
            .skip(start as usize)
            .take(FOCUS_LABEL.len())
            .collect();
        assert_eq!(
            drawn, FOCUS_LABEL,
            "the rectangle must sit on the drawn label"
        );

        assert_eq!(focus_button(&state_at(None), area), None, "owner");
        let project = state_at(Some(project_node()));
        assert_eq!(focus_button(&project, area), None, "project");

        state.set_notice("something happened");
        assert_eq!(focus_button(&state, area), None, "a notice hides the label");
        state.clear_notice();
        state.request_confirm(ConfirmKind::Kill, "s1", "name");
        assert_eq!(focus_button(&state, area), None, "a prompt hides the label");

        // Too narrow to show the label at all.
        let narrow = state_at(Some(session_node()));
        assert_eq!(focus_button(&narrow, Rect::new(0, 0, 5, 1)), None);
    }

    #[test]
    fn bar_text_shows_focus_hint_by_default() {
        let mut state = ProjectCtlState::default();
        assert_eq!(state.focus, Pane::Tree);
        assert!(bar_text(&state).contains(OWNER_HINT));

        state.focus = Pane::Activity;
        assert!(bar_text(&state).contains(ACTIVITY_HINT));
    }

    #[test]
    fn bar_text_prefers_notice_over_hint() {
        let mut state = ProjectCtlState::default();
        state.set_notice("killed foo — now stopped");
        let text = bar_text(&state);
        assert!(text.contains("killed foo — now stopped"));
        assert!(!text.contains(OWNER_HINT));
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
