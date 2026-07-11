//! Cross-module integration tests for the `tm projects` TUI skeleton (#2118).
//!
//! Why: each submodule (`state`, `poll`, `events`, `panes::*`) unit-tests its
//! own pure logic in isolation; this file instead wires them together the way
//! the real run loop does — poll projections feeding state, then key events
//! reading that state — catching seams the per-module tests cannot (mirrors
//! `tui::coordinator::tests` / `tui::health::tests`).
//! What: covers the full poll → state → key-dispatch → pane-render pipeline
//! with synthetic daemon data (no network, no terminal).
//! Test: this IS the test module.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::events::{PendingAction, handle_key};
use super::panes::{actions_bar, projects, sessions};
use super::poll::{project_to_row, session_to_row};
use super::state::{Pane, ProjectCtlState};
use crate::client::{FleetProjectGroupWire, ManagedSessionSummary};
use crate::project::Project;

fn project(name: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: format!("https://github.com/acme/{name}"),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        github: None,
        commit_name: None,
        commit_email: None,
    }
}

fn summary(id: &str, state: &str) -> ManagedSessionSummary {
    ManagedSessionSummary {
        id: id.to_string(),
        name: format!("s-{id}"),
        state: state.to_string(),
        workspace_path: None,
        repo_url: None,
        branch: Some("main".to_string()),
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: Some("ship it".to_string()),
        cwd: None,
        claude_session_id: None,
        pane_id: None,
    }
}

/// Build a state that mirrors what `project_ctl_poll_daemon` would produce
/// from two projects, one of which has a live session — without a network
/// call.
fn seeded_state() -> ProjectCtlState {
    let trusty_tools = project("trusty-tools");
    let genealogy = project("genealogy");
    let group = FleetProjectGroupWire {
        project_name: "trusty-tools".to_string(),
        repo_url: trusty_tools.repo_url.clone(),
        sessions: vec![summary("a1b2c3d4e5f6", "active")],
    };

    let mut state = ProjectCtlState {
        projects: vec![
            project_to_row(&trusty_tools, Some(&group)),
            project_to_row(&genealogy, None),
        ],
        ..Default::default()
    };
    state.sessions_by_project.insert(
        "trusty-tools".to_string(),
        group.sessions.into_iter().map(session_to_row).collect(),
    );
    state.projects_nav.sync_len(state.projects.len());
    state.sessions_nav.sync_len(state.current_sessions().len());
    state
}

#[test]
fn seeded_state_renders_live_project_and_its_session() {
    let state = seeded_state();
    assert_eq!(state.projects.len(), 2);
    assert_eq!(state.selected_project().unwrap().name, "trusty-tools");
    assert_eq!(state.current_sessions().len(), 1);

    let projects_line = projects::project_line(&state.projects[0]);
    let text: String = projects_line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        text.starts_with("●1"),
        "expected a live glyph+count: {text}"
    );

    let session_line = sessions::session_line(1, &state.current_sessions()[0]);
    let text: String = session_line
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(text.contains("a1b2c3d4"), "missing short id: {text}");
    assert!(text.contains("ship it"), "missing task: {text}");
}

#[test]
fn drilling_in_then_killing_an_active_session_requires_confirmation() {
    // The seeded session is "active", so DOC-35 §5.2's confirm gate applies:
    // `k` alone must not fire — it takes drill-in, `k`, THEN `y` to reach the
    // real PendingAction.
    let mut state = seeded_state();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert!(handle_key(&mut state, enter).is_none());
    assert_eq!(state.focus, Pane::Sessions);

    let kill = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
    assert!(
        handle_key(&mut state, kill).is_none(),
        "kill on an Active session must open the confirm gate, not fire"
    );
    assert!(state.pending_confirm.is_some());

    let confirm = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    let action = handle_key(&mut state, confirm);
    assert_eq!(
        action,
        Some(PendingAction::Kill("a1b2c3d4e5f6".to_string()))
    );
    assert!(state.pending_confirm.is_none());
}

#[test]
fn switching_to_the_empty_project_clears_the_sessions_pane() {
    let mut state = seeded_state();
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    handle_key(&mut state, down);
    assert_eq!(state.selected_project().unwrap().name, "genealogy");
    assert!(state.current_sessions().is_empty());
    assert!(state.selected_session().is_none());
}

#[test]
fn action_bar_reflects_daemon_reachability_by_default() {
    let state = seeded_state();
    // Freshly seeded state (no poll ran) starts unreachable, matching the
    // real poller's default until the first health probe succeeds.
    assert!(actions_bar::bar_text(&state).contains(actions_bar::DAEMON_UNREACHABLE));
}
