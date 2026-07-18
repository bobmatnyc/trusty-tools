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
use super::poll::{project_ctl_poll_daemon, project_to_row, session_to_row};
use super::state::{ConfirmKind, DeliverableLinkState, Pane, ProjectCtlState};
use crate::client::{DaemonClient, FleetProjectGroupWire, ManagedSessionSummary};
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
        gh_account: None,
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
        persisted_state: None,
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
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
attached: false,
slot: 0,
deleted: false,
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
        projects_full: [
            ("trusty-tools".to_string(), trusty_tools.clone()),
            ("genealogy".to_string(), genealogy.clone()),
        ]
        .into_iter()
        .collect(),
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

    let session_line = sessions::session_line(
        1,
        &state.current_sessions()[0],
        DeliverableLinkState::Unknown,
    );
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

/// A `127.0.0.1` URL bound to an ephemeral port, then immediately dropped so
/// a later connect is refused — mirrors
/// `tui::coordinator::tests::dead_loopback_url` / `poll::tests::dead_loopback_url`.
fn dead_loopback_url() -> String {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback port");
    let port = listener.local_addr().expect("read bound local addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// A daemon poll racing with an open confirmation gate must never reassign or
/// clear it (DOC-35 §5.2) — the gate pins its target session id at the moment
/// it opens, and a refresh (even one that clears the whole fleet, as a
/// daemon-down poll does) must leave that pin untouched. This is the
/// regression test the #2119 task explicitly calls for on any change that
/// touches the refresh path.
#[tokio::test]
async fn poll_never_touches_pending_confirm() {
    let mut state = seeded_state();
    state.request_confirm(
        ConfirmKind::Decommission,
        "a1b2c3d4e5f6",
        "trusty-tools-session",
    );
    let before = state.pending_confirm.clone().expect("confirm requested");

    let mut client = DaemonClient::new(dead_loopback_url());
    project_ctl_poll_daemon(&mut state, &mut client).await;

    assert_eq!(
        state.pending_confirm,
        Some(before),
        "a poll must never mutate an open confirmation gate's pinned target"
    );
}

/// A daemon poll racing with an open Deliverable/Milestone view must never
/// close it (DOC-35 §10.8, #2383) — mirrors `poll_never_touches_pending_confirm`
/// for the second modal this screen now has. A daemon-down poll MAY leave the
/// view's cached `deliverables`/`milestones` stale (there is no live
/// connection to refresh them from), but must not clear `deliverable_view`
/// itself out from under the operator.
#[tokio::test]
async fn poll_never_closes_an_open_deliverable_view() {
    let mut state = seeded_state();
    state.open_deliverable_view("trusty-tools");
    assert!(state.deliverable_view.is_some());

    let mut client = DaemonClient::new(dead_loopback_url());
    project_ctl_poll_daemon(&mut state, &mut client).await;

    assert_eq!(
        state
            .deliverable_view
            .as_ref()
            .map(|v| v.project_name.as_str()),
        Some("trusty-tools"),
        "a poll must never close an open Deliverable/Milestone view"
    );
}

/// A daemon poll racing with an open config form must never touch it (DOC-35
/// §6, #2120) — the THIRD modal this invariant now covers, mirroring
/// `poll_never_touches_pending_confirm` (which pins by `session_id`) and
/// `poll_never_closes_an_open_deliverable_view` (which pins by
/// `project_name`). Pinned here by `project_name`, same as the Deliverable
/// view. Unlike the Deliverable view, the config form has no live daemon-fed
/// content to refresh mid-edit (`poll.rs` never touches `state.config_form`
/// at all, by construction — see `project_ctl_poll_daemon`'s doc), so this
/// test ALSO asserts the stronger claim the task calls for: an in-progress,
/// UNSAVED field edit survives a poll byte-for-byte, not just "the form
/// stays open."
#[tokio::test]
async fn poll_doesnt_touch_open_config_form() {
    let mut state = seeded_state();
    state.open_config_form("trusty-tools");
    state.config_form.as_mut().unwrap().description.value = "unsaved edit in progress".to_string();
    let before = state.config_form.clone().expect("form requested");

    let mut client = DaemonClient::new(dead_loopback_url());
    project_ctl_poll_daemon(&mut state, &mut client).await;

    assert_eq!(
        state.config_form,
        Some(before),
        "a poll must never mutate an open config form — not even its \
         unsaved, not-yet-submitted field edits"
    );
}
