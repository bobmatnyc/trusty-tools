//! #9000: the TUI offers no resume, stop or delete on an Architect row.
//!
//! Why: the daemon refuses those verbs on a protected record (#8942), so a key
//! or a hint that offers one is a dead end. These tests drive the state machine
//! and a `TestBackend` frame; no daemon is involved.

use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::session_manager::SessionKind;

use super::super::state::{Action, Input, Mode, Severity, TuiState};
use super::{draw, session};

/// Every kind the daemon protects.
const PROTECTED: [SessionKind; 3] = [
    SessionKind::Architect,
    SessionKind::ArchitectAux,
    SessionKind::Unknown,
];

/// A two-row fleet whose first row has `kind` in `state`, browsing row 0.
fn fleet_with(kind: SessionKind, state: &str) -> (Vec<ManagedSessionSummary>, TuiState) {
    let mut first = session("tm-architect", state, 1);
    first.kind = Some(kind);
    let sessions = vec![first, session("tm-bravo-02", "stopped", 2)];
    let mut tui = TuiState::new(None, None);
    tui.sync(&sessions);
    (sessions, tui)
}

#[test]
fn state_enter_on_an_architect_row_sends_no_resume() {
    for kind in PROTECTED {
        let (sessions, mut state) = fleet_with(kind, "active");
        assert_eq!(
            state.apply(Input::Enter, &sessions),
            Action::Redraw,
            "{kind:?}"
        );
        let (text, severity) = state.message().expect("a refusal line");
        assert_eq!(severity, Severity::Error, "{kind:?}");
        assert!(text.contains("tm does not resume it"), "{kind:?}: {text}");
    }
}

#[test]
fn state_delete_on_an_architect_row_opens_no_confirm() {
    for kind in PROTECTED {
        for row_state in ["active", "stopped", "errored"] {
            let (sessions, mut state) = fleet_with(kind, row_state);
            assert_eq!(state.apply(Input::Char('d'), &sessions), Action::Redraw);
            assert_eq!(state.mode(), &Mode::Browse, "{kind:?} {row_state}");
            let (text, _) = state.message().expect("a refusal line");
            assert!(text.contains("tm does not delete it"), "{kind:?}: {text}");
        }
    }
}

#[test]
fn state_ordinary_and_terminal_architect_rows_keep_their_verbs() {
    // The daemon's rule: only a live protected record is refused.
    for (kind, row_state) in [
        (SessionKind::Ordinary, "active"),
        (SessionKind::Architect, "decommissioned"),
    ] {
        let (sessions, mut state) = fleet_with(kind, row_state);
        assert_eq!(state.apply(Input::Enter, &sessions), Action::Open(0));
        state.apply(Input::Char('d'), &sessions);
        assert!(
            matches!(state.mode(), Mode::Confirm { index: 0, .. }),
            "{kind:?} {row_state}: {:?}",
            state.mode()
        );
    }
}

#[test]
fn render_footer_drops_open_and_delete_on_an_architect_row() {
    let (sessions, mut state) = fleet_with(SessionKind::Architect, "active");
    let screen = draw(80, 24, &sessions, &mut state).join("\n");
    assert!(!screen.contains("Enter open"), "{screen}");
    assert!(!screen.contains("d delete"), "{screen}");
    assert!(screen.contains("Architect row: no open/delete"), "{screen}");

    state.apply(Input::Down, &sessions);
    let screen = draw(80, 24, &sessions, &mut state).join("\n");
    assert!(screen.contains("Enter open"), "{screen}");
    assert!(screen.contains("d delete"), "{screen}");
}

#[test]
fn render_help_drops_open_and_delete_on_an_architect_row() {
    let (sessions, mut state) = fleet_with(SessionKind::ArchitectAux, "active");
    state.apply(Input::Char('?'), &sessions);
    let screen = draw(100, 30, &sessions, &mut state).join("\n");
    assert!(!screen.contains("open (resume + attach)"), "{screen}");
    assert!(
        !screen.contains("delete it, with a confirm step"),
        "{screen}"
    );
    assert!(
        screen.contains("not offered on an Architect row"),
        "{screen}"
    );
}
