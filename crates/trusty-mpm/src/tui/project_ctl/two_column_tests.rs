//! Two-column MPM pane tests (#10754 S1).
//!
//! Why: the main row split into an owner/project column and a session column.
//! These tests drive the real key, mouse and render paths and read the drawn
//! buffer, so each one checks what the operator sees.
//! What: (a) both columns at 100x30, (b) an owner's sessions grouped by
//! project, (c) focus moves by Tab, arrows and clicks, (d) the session
//! selection across a repoll, (e) one column below 60 columns, (f) the
//! session verbs act on the right column's session.
//! Test: this file is the test.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};

use super::events::{PendingAction, handle_key, handle_mouse};
use super::layout;
use super::poll::tick::{CoreFetch, apply_core};
use super::state::{ConfirmKind, NodeId, ProjectCtlState, ProjectRow, SessionRow};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(state: &mut ProjectCtlState, code: KeyCode) -> Option<PendingAction> {
    handle_key(state, key(code))
}

fn session(id: &str, name: &str, state: &str, parent: Option<&str>) -> SessionRow {
    SessionRow {
        id: id.to_string(),
        name: name.to_string(),
        state: state.to_string(),
        parent_session_id: parent.map(str::to_string),
        ..Default::default()
    }
}

fn project(name: &str) -> ProjectRow {
    ProjectRow {
        name: name.to_string(),
        repo_url: format!("https://github.com/acme/{name}"),
        ..Default::default()
    }
}

fn project_node(name: &str) -> NodeId {
    NodeId::Project {
        owner: "acme".to_string(),
        name: name.to_string(),
    }
}

fn owner_node() -> NodeId {
    NodeId::Owner("acme".to_string())
}

/// acme/alpha (boss and its worker helper), acme/beta (beta-one), as a poll.
fn fleet(alpha: Vec<SessionRow>) -> CoreFetch {
    let mut sessions = BTreeMap::new();
    sessions.insert("alpha".to_string(), alpha);
    sessions.insert(
        "beta".to_string(),
        vec![session("b1", "beta-one", "active", None)],
    );
    CoreFetch {
        reachable: true,
        fleet: Some((
            vec![project("alpha"), project("beta")],
            sessions,
            BTreeMap::new(),
        )),
    }
}

fn alpha_sessions() -> Vec<SessionRow> {
    vec![
        session("m1", "boss", "active", None),
        session("w1", "helper", "stopped", Some("m1")),
    ]
}

/// The polled fleet with project alpha selected in the left column.
fn polled() -> ProjectCtlState {
    let mut state = ProjectCtlState::default();
    apply_core(&mut state, fleet(alpha_sessions()));
    state.select_node(project_node("alpha"));
    state
}

fn draw(state: &mut ProjectCtlState, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal
        .draw(|f| layout::render(f, state))
        .expect("draw the projects view");
    terminal.backend().buffer().clone()
}

/// The text of `buffer` between columns `x0..x1`, one line per row.
fn region(buffer: &Buffer, x0: u16, x1: u16) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (x0..x1)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first screen cell where `needle` is drawn.
fn find(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    (0..buffer.area.height).find_map(|y| {
        let cells: Vec<&str> = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        let first = needle.chars().next()?.to_string();
        (0..cells.len()).find_map(|x| {
            let rest: String = cells[x..].concat();
            (cells[x] == first && rest.starts_with(needle)).then_some((x as u16, y))
        })
    })
}

/// The text of the cyan (focused) column title on the top row.
fn focused_title(buffer: &Buffer) -> String {
    (0..buffer.area.width)
        .filter(|&x| buffer[(x, 0)].fg == Color::Cyan)
        .map(|x| buffer[(x, 0)].symbol())
        .collect()
}

fn click(state: &mut ProjectCtlState, (column, row): (u16, u16)) {
    handle_mouse(
        state,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn wheel_down(state: &mut ProjectCtlState, (column, row): (u16, u16)) {
    handle_mouse(
        state,
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn selected_id(state: &ProjectCtlState) -> Option<String> {
    state.selected_session().map(|s| s.id.clone())
}

/// (a) At 100x30 the left 30 columns hold owner and project rows only, and
/// the right column holds the selected project's sessions, worker indented.
#[test]
fn render_100x30_shows_both_columns() {
    let mut state = polled();
    let buffer = draw(&mut state, 100, 30);
    let left = region(&buffer, 0, 30);
    let right = region(&buffer, 30, 100);
    assert!(left.contains("acme"), "{left}");
    assert!(left.contains("alpha"), "{left}");
    assert!(left.contains("beta"), "{left}");
    for name in ["boss", "helper", "beta-one"] {
        assert!(!left.contains(name), "{name} in the left column:\n{left}");
    }
    assert!(right.contains("● boss"), "{right}");
    assert!(right.contains("  ↳ ○ helper"), "worker indent:\n{right}");
    assert!(
        !right.contains("beta-one"),
        "only alpha's sessions:\n{right}"
    );
}

/// (b) An owner selected lists all its sessions, grouped under one header
/// line per project, in registry order (Architect Q1).
#[test]
fn owner_selected_groups_sessions_by_project() {
    let mut state = polled();
    state.select_node(owner_node());
    let buffer = draw(&mut state, 100, 30);
    let right = region(&buffer, 30, 100);
    let lines: Vec<&str> = right.lines().map(str::trim).collect();
    let at = |prefix: &str| {
        lines
            .iter()
            .position(|l| l.trim_start_matches(['│', ' ']).starts_with(prefix))
            .unwrap_or_else(|| panic!("no line starting {prefix:?}:\n{right}"))
    };
    let (alpha, boss, helper) = (at("alpha  2 sessions"), at("● boss"), at("↳ ○ helper"));
    let (beta, beta_one) = (at("beta  1 session"), at("● beta-one"));
    assert!(alpha < boss && boss < helper && helper < beta && beta < beta_one);
}

/// (c) Tab cycles Tree, Sessions, Activity; Right on a project enters the
/// session column and Left leaves it; a click focuses the column it hits.
#[test]
fn tab_and_arrows_move_focus_between_columns() {
    let mut state = polled();
    assert!(focused_title(&draw(&mut state, 100, 30)).starts_with("OWNERS / PROJECTS"));
    press(&mut state, KeyCode::Tab);
    let title = focused_title(&draw(&mut state, 100, 30));
    assert!(title.starts_with("SESSIONS — alpha"), "{title:?}");
    assert_eq!(selected_id(&state).as_deref(), Some("m1"), "entry selects");
    press(&mut state, KeyCode::Tab);
    assert_eq!(focused_title(&draw(&mut state, 100, 30)), "", "Activity");
    press(&mut state, KeyCode::Tab);
    assert!(focused_title(&draw(&mut state, 100, 30)).starts_with("OWNERS"));

    press(&mut state, KeyCode::Right);
    assert!(focused_title(&draw(&mut state, 100, 30)).starts_with("SESSIONS"));
    press(&mut state, KeyCode::Left);
    assert!(focused_title(&draw(&mut state, 100, 30)).starts_with("OWNERS"));
    assert_eq!(state.selected_node(), Some(&project_node("alpha")));
}

#[test]
fn click_selects_and_focuses_either_column() {
    let mut state = polled();
    let buffer = draw(&mut state, 100, 30);
    click(
        &mut state,
        find(&buffer, "helper").expect("helper is drawn"),
    );
    assert_eq!(selected_id(&state).as_deref(), Some("w1"));
    assert_eq!(
        state.tree.details.as_deref(),
        Some("w1"),
        "a click is Enter"
    );
    press(&mut state, KeyCode::Esc);
    assert!(focused_title(&draw(&mut state, 100, 30)).starts_with("SESSIONS"));

    let buffer = draw(&mut state, 100, 30);
    click(&mut state, find(&buffer, "· beta").expect("beta is drawn"));
    assert_eq!(state.selected_node(), Some(&project_node("beta")));
    let buffer = draw(&mut state, 100, 30);
    assert!(focused_title(&buffer).starts_with("OWNERS"));
    assert!(
        region(&buffer, 30, 100).contains("beta-one"),
        "beta's sessions"
    );
}

#[test]
fn wheel_scrolls_the_column_under_the_cursor() {
    let mut state = polled();
    let buffer = draw(&mut state, 100, 30);
    wheel_down(&mut state, find(&buffer, "boss").expect("boss is drawn"));
    assert_eq!(selected_id(&state).as_deref(), Some("m1"), "first session");
    wheel_down(&mut state, find(&buffer, "boss").expect("boss is drawn"));
    assert_eq!(selected_id(&state).as_deref(), Some("w1"));
    assert_eq!(
        state.selected_node(),
        Some(&project_node("alpha")),
        "the left column kept its selection"
    );
    wheel_down(
        &mut state,
        find(&buffer, "· alpha").expect("alpha is drawn"),
    );
    assert_eq!(state.selected_node(), Some(&project_node("beta")));
}

/// (d) The right column's selection is a session id: a repoll that reorders
/// the sessions keeps it, and one that drops it clamps to a neighbour.
#[test]
fn session_selection_survives_repoll_and_clamps_when_it_vanishes() {
    let mut state = polled();
    assert!(state.focus_session("w1"));
    let mut reordered = alpha_sessions();
    reordered.insert(0, session("n1", "newcomer", "active", None));
    apply_core(&mut state, fleet(reordered));
    assert_eq!(selected_id(&state).as_deref(), Some("w1"), "kept by id");

    apply_core(
        &mut state,
        fleet(vec![session("m1", "boss", "active", None)]),
    );
    assert_eq!(
        selected_id(&state).as_deref(),
        Some("m1"),
        "a vanished session clamps to the nearest row left"
    );
    assert_eq!(state.selected_node(), Some(&project_node("alpha")));

    apply_core(&mut state, fleet(Vec::new()));
    assert_eq!(selected_id(&state), None, "nothing left to select");
    let right = region(&draw(&mut state, 100, 30), 30, 100);
    assert!(
        right.contains("no sessions in alpha"),
        "empty hint:\n{right}"
    );
}

/// (e) Below 60 columns only the focused column draws, at full width.
#[test]
fn narrow_terminal_renders_only_the_focused_column() {
    let mut state = polled();
    let buffer = draw(&mut state, 50, 30);
    let all = region(&buffer, 0, 50);
    assert!(all.contains("OWNERS / PROJECTS"), "{all}");
    assert!(!all.contains("SESSIONS"), "one column:\n{all}");
    assert!(!all.contains("boss"), "{all}");
    assert_eq!(buffer[(49, 0)].symbol(), "┐", "the tree fills the width");

    press(&mut state, KeyCode::Tab);
    let buffer = draw(&mut state, 50, 30);
    let all = region(&buffer, 0, 50);
    assert!(all.contains("SESSIONS — alpha"), "{all}");
    assert!(!all.contains("OWNERS"), "one column:\n{all}");
    assert!(all.contains("boss"), "{all}");
    assert_eq!(buffer[(0, 0)].symbol(), "┌", "the sessions fill the width");
}

/// (f) The session verbs act on the session selected in the right column,
/// through the same gates; in the left column they act on nothing.
#[test]
fn session_verbs_act_on_the_right_column_session() {
    let mut state = polled();
    press(&mut state, KeyCode::Right);
    press(&mut state, KeyCode::Down);
    assert!(focused_title(&draw(&mut state, 100, 30)).starts_with("SESSIONS"));
    assert_eq!(selected_id(&state).as_deref(), Some("w1"));

    assert_eq!(
        press(&mut state, KeyCode::Char('a')),
        Some(PendingAction::Attach("w1".to_string()))
    );
    assert_eq!(
        press(&mut state, KeyCode::Char('r')),
        Some(PendingAction::Resume("w1".to_string()))
    );
    assert_eq!(
        press(&mut state, KeyCode::Char('k')),
        Some(PendingAction::Kill("w1".to_string())),
        "a stopped session is killed without the gate"
    );
    press(&mut state, KeyCode::Char('d'));
    let confirm = state.pending_confirm.take().expect("decommission is gated");
    assert_eq!(
        (confirm.kind, confirm.session_id.as_str()),
        (ConfirmKind::Decommission, "w1")
    );

    press(&mut state, KeyCode::Up);
    press(&mut state, KeyCode::Char('k'));
    let confirm = state
        .pending_confirm
        .take()
        .expect("an active kill is gated");
    assert_eq!(
        (confirm.kind, confirm.session_id.as_str()),
        (ConfirmKind::Kill, "m1")
    );

    press(&mut state, KeyCode::Left);
    for verb in ['k', 'r', 'd', 'a', 'f'] {
        assert_eq!(press(&mut state, KeyCode::Char(verb)), None, "{verb}");
        assert!(state.pending_confirm.is_none(), "{verb} opened a gate");
    }
    assert_eq!(state.notice.as_deref(), Some("select a session first"));
}
