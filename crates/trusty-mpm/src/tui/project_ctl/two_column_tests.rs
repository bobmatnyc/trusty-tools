//! Two-column MPM pane tests (#10754 S1, S2).
//!
//! Why: the main row split into an owner/project column and a session column.
//! These tests drive the real key, mouse and render paths and read the drawn
//! buffer, so each one checks what the operator sees.
//! What: S1 — (a) both columns at 100x30, (b) an owner's sessions grouped by
//! project, (c) focus moves by Tab, arrows and clicks, (d) the session
//! selection across a repoll, (e) one column below 60 columns, (f) the
//! session verbs act on the right column's session. S2 — a session row
//! expands in place (render, repoll, keys, clicks, no popup) and the
//! session order does not follow the poll order; plus the column's edge
//! cases (headers, blank clicks, narrow widths). S3 — #10754: De1 key map
//! (Bob 11-06): Right toggles details and Enter does not; `r` renames
//! through a pre-filled modal; `n` starts a new session.
//! Test: this file is the test.

use std::collections::BTreeMap;

use crate::tui::theme::palette;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

use super::events::{PendingAction, handle_key, handle_mouse};
use super::layout;
use super::poll::tick::{CoreFetch, apply_core};
use super::state::{
    ConfirmKind, NodeId, Pane, ProjectCtlState, ProjectRow, SessionColRow, SessionRow,
};

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

/// The text of the accent (focused) column title on the top row.
fn focused_title(buffer: &Buffer) -> String {
    (0..buffer.area.width)
        .filter(|&x| buffer[(x, 0)].fg == palette().accent)
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
    assert!(
        state.session_col.expanded.contains("w1"),
        "a click is Enter"
    );
    press(&mut state, KeyCode::Esc);
    assert!(state.session_col.expanded.is_empty(), "Esc collapses");
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
        press(&mut state, KeyCode::Char('R')),
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

/// The lines of the right column (columns 30..100) of a 100-wide frame.
fn right_lines(state: &mut ProjectCtlState, height: u16) -> Vec<String> {
    let buffer = draw(state, 100, height);
    region(&buffer, 30, 100)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The index of the first line containing `needle`.
fn line_of(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?}:\n{}", lines.join("\n")))
}

/// S2 (a): Right on a session expands it in place (#10754: De1 key map (Bob
/// 11-06), Enter before). Its fields draw on the lines under its row, inside
/// the right column; a second Right collapses it.
#[test]
fn right_expands_a_session_and_details_render_under_it() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    assert_eq!(selected_id(&state).as_deref(), Some("m1"));
    assert_eq!(press(&mut state, KeyCode::Right), None, "no daemon call");
    let buffer = draw(&mut state, 100, 30);
    assert!(
        !region(&buffer, 0, 30).contains("id:"),
        "nothing spills into the left column"
    );
    let lines = right_lines(&mut state, 30);
    let boss = line_of(&lines, "● boss");
    let fields = [
        "name:        boss",
        "id:          m1",
        "project:     alpha",
        "state:       active",
        "branch:      -",
        "task:        -",
        "parent:      -",
        "decision:    -",
        "deliverable: -",
        "workspace:   -",
        "last active: -",
    ];
    for (offset, field) in fields.iter().enumerate() {
        let line = &lines[boss + 1 + offset];
        assert!(line.contains(field), "{field:?} on {line:?}");
        assert!(
            line.starts_with("│    "),
            "indented under the row: {line:?}"
        );
    }
    assert!(lines[boss + 12].contains("↳ ○ helper"), "{lines:#?}");

    press(&mut state, KeyCode::Right);
    let lines = right_lines(&mut state, 30);
    assert!(lines[line_of(&lines, "● boss") + 1].contains("↳ ○ helper"));
    assert!(!lines.join("\n").contains("id:"), "collapsed");
}

/// S2 (b): the expansion is keyed by session id. A repoll that reorders the
/// input keeps it; a session that vanishes drops out and comes back closed.
#[test]
fn expansion_survives_a_reordering_repoll() {
    let mut state = polled();
    assert!(state.focus_session("w1"));
    press(&mut state, KeyCode::Right);
    let mut reordered = alpha_sessions();
    reordered.reverse();
    reordered.insert(0, session("n1", "newcomer", "active", None));
    apply_core(&mut state, fleet(reordered));
    assert_eq!(selected_id(&state).as_deref(), Some("w1"));
    let lines = right_lines(&mut state, 30);
    let helper = line_of(&lines, "↳ ○ helper");
    assert!(lines[helper + 2].contains("id:          w1"), "{lines:#?}");
    assert!(
        !lines.join("\n").contains("id:          m1"),
        "only w1 opened"
    );

    apply_core(
        &mut state,
        fleet(vec![session("m1", "boss", "active", None)]),
    );
    apply_core(&mut state, fleet(alpha_sessions()));
    let lines = right_lines(&mut state, 30);
    assert!(!lines.join("\n").contains("id:"), "w1 returned closed");
}

/// S2 (c): detail lines cannot be selected. Up and Down step over them, and
/// the selected row stays drawn when the rows above it expand.
#[test]
fn up_and_down_skip_detail_lines() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    press(&mut state, KeyCode::Right);
    press(&mut state, KeyCode::Down);
    assert_eq!(
        selected_id(&state).as_deref(),
        Some("w1"),
        "over m1's details"
    );
    press(&mut state, KeyCode::Down);
    assert_eq!(selected_id(&state).as_deref(), Some("w1"), "saturates");
    // A short frame: m1's 11 detail lines push helper below the fold.
    let lines = right_lines(&mut state, 14);
    assert!(lines.join("\n").contains("helper"), "{lines:#?}");
    // Expanding the bottom row brings its first fields into view.
    press(&mut state, KeyCode::Right);
    let lines = right_lines(&mut state, 14);
    assert!(lines.join("\n").contains("id:          w1"), "{lines:#?}");
    press(&mut state, KeyCode::Up);
    assert_eq!(
        selected_id(&state).as_deref(),
        Some("m1"),
        "over the details"
    );
    // Left collapses an expanded row before it leaves the column.
    press(&mut state, KeyCode::Left);
    assert!(
        !right_lines(&mut state, 30)
            .join("\n")
            .contains("id:          m1")
    );
    press(&mut state, KeyCode::Esc);
    assert!(
        right_lines(&mut state, 30)
            .join("\n")
            .contains("id:          w1"),
        "Esc on m1 leaves w1 open"
    );
}

/// S2 (d): a click on a detail line selects its session without collapsing
/// it; a click on a session row toggles that row.
#[test]
fn clicking_a_detail_line_selects_its_session() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    press(&mut state, KeyCode::Right);
    assert!(state.focus_session("w1"));
    let buffer = draw(&mut state, 100, 30);
    click(
        &mut state,
        find(&buffer, "id:          m1").expect("m1's id"),
    );
    assert_eq!(selected_id(&state).as_deref(), Some("m1"));
    let buffer = draw(&mut state, 100, 30);
    assert!(find(&buffer, "id:          m1").is_some(), "still open");

    click(
        &mut state,
        find(&buffer, "helper").expect("helper is drawn"),
    );
    assert_eq!(selected_id(&state).as_deref(), Some("w1"));
    let buffer = draw(&mut state, 100, 30);
    assert!(
        find(&buffer, "id:          w1").is_some(),
        "a row click opens"
    );
    click(
        &mut state,
        find(&buffer, "helper").expect("helper is drawn"),
    );
    let buffer = draw(&mut state, 100, 30);
    assert!(find(&buffer, "id:          w1").is_none(), "and closes");
}

/// S2 (e): the modal overlay is gone. Right draws no popup and leaves every
/// other key live, quit included.
#[test]
fn right_never_opens_a_popup() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    press(&mut state, KeyCode::Right);
    let buffer = draw(&mut state, 100, 30);
    assert!(find(&buffer, "Session — boss").is_none(), "no popup title");
    assert!(find(&buffer, "[Esc] close").is_none(), "no popup keys");
    press(&mut state, KeyCode::Char('q'));
    assert!(state.should_exit, "no modal swallows quit");
}

/// S2 (f): the rows do not follow the poll order. Every shuffle of the same
/// sessions draws managers before their workers, then by name, then by id.
#[test]
fn session_order_is_stable_when_the_input_is_shuffled() {
    let sessions = vec![
        session("m1", "boss", "active", None),
        session("w1", "helper", "stopped", Some("m1")),
        session("m2", "alice", "active", None),
        session("w2", "zed", "active", Some("m2")),
        session("m0", "boss", "active", None),
    ];
    let expected = ["m2", "w2", "m0", "m1", "w1"];
    for shift in 0..sessions.len() {
        for reverse in [false, true] {
            let mut input = sessions.clone();
            input.rotate_left(shift);
            if reverse {
                input.reverse();
            }
            let mut state = ProjectCtlState::default();
            apply_core(&mut state, fleet(input));
            state.select_node(project_node("alpha"));
            let ids: Vec<&str> = state
                .session_col
                .rows
                .iter()
                .filter_map(|r| r.session_id())
                .collect();
            assert_eq!(ids, expected, "shift {shift}, reverse {reverse}");
        }
    }
}

/// One step of [`session_column_edge_cases`].
enum Edge {
    /// Press Down in the session column.
    Down,
    /// Click the line just below the last row.
    ClickBelowLastRow,
    /// Click the `beta` project header.
    ClickHeader,
    /// Draw at this width and 30 rows.
    Render(u16),
}

/// #10754 S1 critic LOWs: in the owner view Down skips the project headers,
/// a click below the last row or on a header changes nothing, and the pane
/// draws at widths 0, 23 and 59 (one column).
#[test]
fn session_column_edge_cases() {
    let mut state = polled();
    state.select_node(owner_node());
    press(&mut state, KeyCode::Tab);
    assert_eq!(
        selected_id(&state).as_deref(),
        Some("m1"),
        "past alpha's header"
    );
    let cases = [
        (Edge::Down, "w1"),
        (Edge::Down, "b1"),
        (Edge::Down, "b1"),
        (Edge::ClickBelowLastRow, "b1"),
        (Edge::ClickHeader, "b1"),
        (Edge::Render(0), "b1"),
        (Edge::Render(23), "b1"),
        (Edge::Render(59), "b1"),
    ];
    for (step, want) in cases {
        let label = match &step {
            Edge::Down => "Down".to_string(),
            Edge::ClickBelowLastRow => "click below".to_string(),
            Edge::ClickHeader => "click header".to_string(),
            Edge::Render(w) => format!("render {w}"),
        };
        match step {
            Edge::Down => {
                press(&mut state, KeyCode::Down);
            }
            Edge::ClickBelowLastRow => {
                draw(&mut state, 100, 30);
                let rows = state.tree.hit.session_rows;
                let below = rows.y + state.session_col.rows.len() as u16;
                assert!(below < rows.bottom(), "the frame has a blank line");
                click(&mut state, (rows.x + 2, below));
            }
            Edge::ClickHeader => {
                // The tree draws the same counters; aim at the right column.
                draw(&mut state, 100, 30);
                let header = state
                    .session_col
                    .rows
                    .iter()
                    .position(
                        |r| matches!(r, SessionColRow::Header { project, .. } if project == "beta"),
                    )
                    .expect("beta's header");
                let rows = state.tree.hit.session_rows;
                click(&mut state, (rows.x + 2, rows.y + header as u16));
            }
            Edge::Render(width) => {
                let buffer = draw(&mut state, width, 30);
                assert_eq!(buffer.area.width, width);
                if width > 0 {
                    let all = region(&buffer, 0, width);
                    assert!(all.contains("SESSIONS"), "focused column:\n{all}");
                    assert!(!all.contains("OWNERS"), "one column:\n{all}");
                }
            }
        }
        assert_eq!(selected_id(&state).as_deref(), Some(want), "{label}");
        assert_eq!(state.focus, super::state::Pane::Sessions, "{label}");
    }
}

/// S3 (b): Right toggles the details; Enter opens the session instead and
/// leaves them alone.
#[test]
fn right_toggles_details_and_enter_does_not() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    assert_eq!(
        press(&mut state, KeyCode::Enter),
        Some(PendingAction::OpenSession("m1".to_string()))
    );
    assert!(!right_lines(&mut state, 30).join("\n").contains("id:"));
    press(&mut state, KeyCode::Right);
    assert!(
        right_lines(&mut state, 30)
            .join("\n")
            .contains("id:          m1")
    );
    assert_eq!(
        press(&mut state, KeyCode::Enter),
        Some(PendingAction::OpenSession("m1".to_string()))
    );
    assert!(
        right_lines(&mut state, 30)
            .join("\n")
            .contains("id:          m1"),
        "Enter leaves the details open"
    );
    press(&mut state, KeyCode::Right);
    assert!(!right_lines(&mut state, 30).join("\n").contains("id:"));
}

/// S3 (c): `r` opens the rename modal with the current name typed. Typing
/// edits it, and Enter submits `RenameSession(id, new)`.
#[test]
fn r_opens_the_rename_modal_prefilled_and_enter_submits() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    assert_eq!(press(&mut state, KeyCode::Char('r')), None, "no call yet");
    let buffer = draw(&mut state, 100, 30);
    assert!(
        find(&buffer, "Rename — boss").is_some(),
        "the modal is drawn"
    );
    assert!(find(&buffer, "new:     boss_").is_some(), "pre-filled");
    // The modal takes every key: `q` and `k` type, they do not act.
    for c in ['-', 'q', 'k'] {
        assert_eq!(press(&mut state, KeyCode::Char(c)), None);
    }
    assert!(!state.should_exit && state.pending_confirm.is_none());
    press(&mut state, KeyCode::Backspace);
    assert_eq!(
        press(&mut state, KeyCode::Enter),
        Some(PendingAction::RenameSession(
            "m1".to_string(),
            "boss-q".to_string()
        ))
    );
    let buffer = draw(&mut state, 100, 30);
    assert!(find(&buffer, "Rename — boss").is_none(), "the modal closed");
}

/// S3 (c): Esc closes the rename modal and dispatches nothing.
#[test]
fn esc_closes_the_rename_modal_with_no_action() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    press(&mut state, KeyCode::Char('r'));
    press(&mut state, KeyCode::Char('x'));
    assert_eq!(press(&mut state, KeyCode::Esc), None);
    let buffer = draw(&mut state, 100, 30);
    assert!(find(&buffer, "Rename — boss").is_none(), "closed");
    assert_eq!(state.notice.as_deref(), Some("rename cancelled"));
    assert_eq!(
        press(&mut state, KeyCode::Char('a')),
        Some(PendingAction::Attach("m1".to_string()))
    );
}

/// S3 (f): `n` starts a new session in the left column's project, or, with an
/// owner selected, in the selected session's project.
#[test]
fn n_starts_a_new_session_in_the_project_from_either_column() {
    let mut state = polled();
    assert_eq!(
        press(&mut state, KeyCode::Char('n')),
        Some(PendingAction::NewSession("alpha".to_string()))
    );
    state.select_node(owner_node());
    assert_eq!(press(&mut state, KeyCode::Char('n')), None);
    assert_eq!(state.notice.as_deref(), Some("no project selected"));
    press(&mut state, KeyCode::Tab);
    for _ in 0..4 {
        press(&mut state, KeyCode::Down);
    }
    assert_eq!(selected_id(&state).as_deref(), Some("b1"), "beta's session");
    assert_eq!(
        press(&mut state, KeyCode::Char('n')),
        Some(PendingAction::NewSession("beta".to_string()))
    );
}

/// #10754: `n` in an EMPTY session column starts a new session in the
/// project selected in the tree, `l` opens the launch form on it, and the
/// empty column advertises `[n]`.
#[test]
fn n_in_an_empty_session_column_starts_a_new_session_and_l_opens_the_form() {
    let mut fetch = fleet(alpha_sessions());
    if let Some((projects, _, _)) = fetch.fleet.as_mut() {
        projects.push(project("qaempty"));
    }
    let mut state = ProjectCtlState::default();
    apply_core(&mut state, fetch);
    state.select_node(project_node("qaempty"));
    press(&mut state, KeyCode::Tab);
    assert!(state.session_col.rows.is_empty(), "qaempty has no sessions");
    assert_eq!(selected_id(&state), None);
    assert_eq!(
        super::panes::session_list::empty_hint(&state),
        "no sessions in qaempty — [n] new session"
    );
    state.set_notice("stale notice");
    assert_eq!(
        press(&mut state, KeyCode::Char('n')),
        Some(PendingAction::NewSession("qaempty".to_string())),
        "n in the empty column"
    );
    assert_eq!(press(&mut state, KeyCode::Char('l')), None);
    let form = state.launch.take().expect("l opens the launch form");
    assert_eq!(form.project, "qaempty");
    // The tree's own `l` takes the same path.
    press(&mut state, KeyCode::BackTab);
    assert_eq!(press(&mut state, KeyCode::Char('l')), None);
    assert_eq!(
        state.launch.take().map(|f| f.project).as_deref(),
        Some("qaempty")
    );
}

/// See #10754: `l` on a session row opens the launch form for its project;
/// the form takes every key, and Enter submits project, task and ref.
#[test]
fn l_opens_the_launch_form_and_enter_submits_the_request() {
    use super::state::launch::LaunchRequest;

    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    assert_eq!(press(&mut state, KeyCode::Char('l')), None, "no call yet");
    let buffer = draw(&mut state, 100, 30);
    assert!(
        find(&buffer, "Launch — alpha").is_some(),
        "the form is drawn"
    );
    assert!(find(&buffer, "> task: _").is_some(), "Task has the cursor");
    // The form takes every key: `q`, `k` and `n` type, they do not act.
    for c in "qkn fix".chars() {
        assert_eq!(press(&mut state, KeyCode::Char(c)), None);
    }
    assert!(!state.should_exit && state.pending_confirm.is_none());
    press(&mut state, KeyCode::Tab);
    for c in "dev".chars() {
        press(&mut state, KeyCode::Char(c));
    }
    assert!(find(&draw(&mut state, 100, 30), "> ref:  dev_").is_some());
    assert_eq!(
        press(&mut state, KeyCode::Enter),
        Some(PendingAction::Launch(LaunchRequest {
            project: "alpha".to_string(),
            task: "qkn fix".to_string(),
            git_ref: Some("dev".to_string()),
        }))
    );
    let buffer = draw(&mut state, 100, 30);
    assert!(find(&buffer, "Launch — alpha").is_none(), "the form closed");
}

/// See #10754: Enter on an empty task keeps the form open with the refusal
/// and sends nothing; Esc closes it with "launch cancelled".
#[test]
fn launch_form_refuses_an_empty_task_and_esc_cancels() {
    let mut state = polled();
    press(&mut state, KeyCode::Char('l'));
    assert_eq!(press(&mut state, KeyCode::Enter), None, "no request sent");
    assert!(state.launch.is_some(), "the form stays open");
    let buffer = draw(&mut state, 100, 30);
    assert!(
        find(&buffer, "a task is required").is_some(),
        "inline refusal"
    );
    assert_eq!(press(&mut state, KeyCode::Esc), None);
    assert!(state.launch.is_none(), "closed");
    assert_eq!(state.notice.as_deref(), Some("launch cancelled"));
}

/// #10754: with focus in the activity pane, `n` starts a new session in the
/// tree's project, `l` opens the launch form on it, and any other unbound key
/// says where to go.
#[test]
fn activity_focus_n_starts_a_new_session_l_launches_and_other_keys_notice() {
    let mut state = polled();
    press(&mut state, KeyCode::Tab);
    press(&mut state, KeyCode::Tab);
    assert_eq!(state.focus, Pane::Activity);
    assert_eq!(
        press(&mut state, KeyCode::Char('n')),
        Some(PendingAction::NewSession("alpha".to_string())),
        "n with activity focus"
    );
    assert_eq!(press(&mut state, KeyCode::Char('l')), None);
    assert_eq!(
        state.launch.take().map(|f| f.project).as_deref(),
        Some("alpha")
    );
    for code in [KeyCode::Char('x'), KeyCode::Char('k'), KeyCode::Enter] {
        state.clear_notice();
        assert_eq!(press(&mut state, code), None, "{code:?}");
        let notice = state.notice.clone().unwrap_or_default();
        assert!(
            notice.contains("Tab to the projects or sessions pane"),
            "{code:?}: {notice}"
        );
    }
    // Navigation keys stay silent; Esc, Tab and quit keep their meaning.
    state.clear_notice();
    assert_eq!(press(&mut state, KeyCode::Down), None);
    assert_eq!(state.notice, None, "Down is not an error");
    press(&mut state, KeyCode::Char('x'));
    press(&mut state, KeyCode::Esc);
    assert_eq!(state.notice, None, "Esc clears the notice");
    assert_eq!(state.focus, Pane::Activity);
    // No project selected: the same notice the other panes give.
    state.select_node(owner_node());
    assert_eq!(press(&mut state, KeyCode::Char('n')), None);
    assert_eq!(state.notice.as_deref(), Some("no project selected"));
    press(&mut state, KeyCode::Tab);
    assert_eq!(state.focus, Pane::Tree, "Tab still cycles out");
}
