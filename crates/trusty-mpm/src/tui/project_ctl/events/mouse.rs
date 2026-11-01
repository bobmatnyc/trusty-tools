//! Mouse input for the projects view (#10476).
//!
//! Why: the owner asked that a click act like Enter and that a Focus button act
//! like `f` (#10476 comment 2). The handler stays pure, like `handle_key`, so
//! every branch is testable with synthetic events.
//! What: [`handle_mouse`] reads the rectangles the last frame recorded in
//! [`HitMap`] and routes one event. A click on a tree row selects it and then
//! does what Enter does. A click on a focus button sends an `f` key through
//! [`handle_key`], so the two cannot diverge. The wheel moves the selection.
//! Everything else does nothing: a blank row, another pane, a button release,
//! and any event under a modal. The details overlay is the one modal that takes
//! a click, on its own focus button only.
//! Test: `tests` below.
//!
//! [`HitMap`]: crate::tui::project_ctl::state::HitMap

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::{PendingAction, handle_key, tree};
use crate::tui::project_ctl::state::{Pane, ProjectCtlState};

/// The tree row drawn at `(column, row)`, by index into the visible rows.
///
/// What: `rows` is the list's inner rectangle (borders excluded) and `offset`
/// the list's scroll offset. `None` outside `rows`. The result may be past the
/// last row; the caller checks that against the row count.
/// Test: `row_at_maps_y_to_row_with_scroll`, `row_at_none_outside_rect`.
pub(crate) fn row_at(rows: Rect, offset: usize, column: u16, row: u16) -> Option<usize> {
    rows.contains(Position::new(column, row))
        .then(|| offset + usize::from(row - rows.y))
}

/// Is `(column, row)` on a live focus button?
///
/// While the details overlay covers the point, only the overlay's own button
/// counts; the action bar may lie under it on a short terminal.
fn on_focus_button(state: &ProjectCtlState, column: u16, row: u16) -> bool {
    let hit = state.tree.hit;
    let at = Position::new(column, row);
    let inside = |rect: Option<Rect>| rect.is_some_and(|r| r.contains(at));
    if inside(hit.details) {
        inside(hit.details_focus)
    } else {
        inside(hit.bar_focus)
    }
}

/// Route one mouse event, returning any action that needs a daemon call.
///
/// Why: see the module doc. The hit map describes the last drawn frame, and the
/// run loop draws between any two events, so a click always maps to what the
/// operator saw.
/// What: see the module doc.
/// Test: `click_owner_toggles`, `click_project_toggles`,
/// `click_session_opens_details`, `click_blank_space_ignored`,
/// `click_outside_pane_ignored`, `click_under_modal_ignored`,
/// `click_focus_button_returns_focus`, `wheel_moves_selection`,
/// `click_hits_the_drawn_row_when_scrolled`.
pub fn handle_mouse(state: &mut ProjectCtlState, mouse: MouseEvent) -> Option<PendingAction> {
    if state.pending_confirm.is_some()
        || state.deliverable_view.is_some()
        || state.config_form.is_some()
    {
        return None;
    }
    let (column, row) = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if on_focus_button(state, column, row) {
                return handle_key(state, KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
            }
            if state.tree.details.is_some() {
                return None;
            }
            let index = row_at(
                state.tree.hit.tree_rows,
                state.tree.nav.offset(),
                column,
                row,
            )?;
            if state.select_row(index) {
                state.focus = Pane::Tree;
                tree::enter(state);
            }
            None
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let in_tree = state
                .tree
                .hit
                .tree_rows
                .contains(Position::new(column, row));
            if in_tree && state.tree.details.is_none() {
                state.focus = Pane::Tree;
                state.tree_step(mouse.kind == MouseEventKind::ScrollDown);
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use crate::tui::project_ctl::layout;
    use crate::tui::project_ctl::state::{ConfirmKind, NodeId, ProjectRow, SessionRow};
    use ratatui::{Terminal, backend::TestBackend};

    fn owner() -> NodeId {
        NodeId::Owner("acme".to_string())
    }

    fn project(name: &str) -> NodeId {
        NodeId::Project {
            owner: "acme".to_string(),
            name: name.to_string(),
        }
    }

    fn session(id: &str) -> NodeId {
        NodeId::Session {
            owner: "acme".to_string(),
            project: "alpha".to_string(),
            id: id.to_string(),
        }
    }

    /// acme/alpha with `sessions` sessions, opened, with the owner selected.
    fn state_with(sessions: usize) -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![ProjectRow {
                name: "alpha".to_string(),
                repo_url: "https://github.com/acme/alpha".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        state.sessions_by_project.insert(
            "alpha".to_string(),
            (0..sessions)
                .map(|i| SessionRow {
                    id: format!("s{i}"),
                    name: format!("name-{i}"),
                    state: "active".to_string(),
                    ..Default::default()
                })
                .collect(),
        );
        state.select_node(project("alpha"));
        state.tree.expanded.insert(project("alpha"));
        state.rebuild_tree();
        state.select_node(owner());
        state
    }

    /// Draw one frame, so the hit map and the list's offset are real.
    fn draw(state: &mut ProjectCtlState, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal
            .draw(|f| layout::render(f, state))
            .expect("draw the projects view");
        terminal
    }

    fn at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(state: &mut ProjectCtlState, column: u16, row: u16) -> Option<PendingAction> {
        let event = at(MouseEventKind::Down(MouseButton::Left), column, row);
        handle_mouse(state, event)
    }

    /// The screen row showing tree row `index`, per the last frame.
    fn screen_row(state: &ProjectCtlState, index: usize) -> u16 {
        state.tree.hit.tree_rows.y + (index - state.tree.nav.offset()) as u16
    }

    fn index_of(state: &ProjectCtlState, id: &NodeId) -> usize {
        state
            .tree
            .rows
            .iter()
            .position(|r| &r.id == id)
            .expect("a visible row")
    }

    #[test]
    fn row_at_maps_y_to_row_with_scroll() {
        let rows = Rect::new(1, 1, 10, 5);
        assert_eq!(row_at(rows, 0, 1, 1), Some(0));
        assert_eq!(row_at(rows, 0, 10, 5), Some(4));
        assert_eq!(row_at(rows, 7, 5, 1), Some(7));
        assert_eq!(row_at(rows, 7, 5, 3), Some(9));
    }

    #[test]
    fn row_at_none_outside_rect() {
        let rows = Rect::new(1, 1, 10, 5);
        assert_eq!(row_at(rows, 0, 0, 2), None, "left border");
        assert_eq!(row_at(rows, 0, 11, 2), None, "right border");
        assert_eq!(row_at(rows, 0, 5, 0), None, "top border");
        assert_eq!(row_at(rows, 0, 5, 6), None, "below the list");
    }

    #[test]
    fn click_owner_toggles() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let row = screen_row(&state, index_of(&state, &owner()));
        assert!(state.tree.expanded.contains(&owner()));
        assert_eq!(click(&mut state, 5, row), None);
        assert!(!state.tree.expanded.contains(&owner()), "click closes");
        draw(&mut state, 80, 24);
        click(&mut state, 5, row);
        assert!(state.tree.expanded.contains(&owner()), "click reopens");
    }

    #[test]
    fn click_project_toggles() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let row = screen_row(&state, index_of(&state, &project("alpha")));
        assert_eq!(click(&mut state, 5, row), None);
        assert_eq!(state.selected_node(), Some(&project("alpha")));
        assert!(!state.tree.expanded.contains(&project("alpha")));
    }

    #[test]
    fn click_session_opens_details() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let row = screen_row(&state, index_of(&state, &session("s1")));
        assert_eq!(click(&mut state, 5, row), None, "details need no daemon");
        assert_eq!(state.selected_node(), Some(&session("s1")));
        assert_eq!(state.tree.details.as_deref(), Some("s1"));
    }

    #[test]
    fn click_blank_space_ignored() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let before = state.selected_node().cloned();
        let expanded = state.tree.expanded.clone();
        // Rows 0-3 hold owner, project and two sessions; the next one is blank.
        let blank = screen_row(&state, state.tree.rows.len());
        assert_eq!(click(&mut state, 5, blank), None);
        assert_eq!(state.selected_node().cloned(), before);
        assert_eq!(state.tree.expanded, expanded);
        assert_eq!(state.tree.details, None);
    }

    #[test]
    fn click_outside_pane_ignored() {
        let mut state = state_with(2);
        state.focus = Pane::Activity;
        draw(&mut state, 80, 24);
        let before = state.selected_node().cloned();
        let tree = state.tree.hit.tree_rows;
        // The Activity strip, the tree's own top border, and its left border.
        for (column, row) in [
            (5, tree.bottom() + 2),
            (5, tree.y - 1),
            (tree.x - 1, tree.y),
        ] {
            assert_eq!(click(&mut state, column, row), None);
        }
        assert_eq!(state.selected_node().cloned(), before);
        assert_eq!(state.focus, Pane::Activity, "no click reached the tree");
    }

    #[test]
    fn click_under_modal_ignored() {
        let open_modals: [fn(&mut ProjectCtlState); 3] = [
            |s| s.request_confirm(ConfirmKind::Kill, "s0", "name-0"),
            |s| s.open_deliverable_view("alpha"),
            |s| s.open_config_form("alpha"),
        ];
        for open in open_modals {
            let mut state = state_with(2);
            state.projects_full.insert(
                "alpha".to_string(),
                Project {
                    name: "alpha".to_string(),
                    repo_url: "https://github.com/acme/alpha".to_string(),
                    default_branch: "main".to_string(),
                    stack_hint: None,
                    tags: vec![],
                    description: None,
                    gh_user: None,
                    gh_account: None,
                    github: None,
                    commit_name: None,
                    commit_email: None,
                    worktree: None,
                    project_key: None,
                },
            );
            state.select_node(session("s0"));
            draw(&mut state, 80, 24);
            let row = screen_row(&state, index_of(&state, &session("s1")));
            let button = state.tree.hit.bar_focus.expect("the bar's button");
            open(&mut state);
            assert!(
                state.pending_confirm.is_some()
                    || state.deliverable_view.is_some()
                    || state.config_form.is_some(),
                "the modal must really be open"
            );

            assert_eq!(click(&mut state, 5, row), None);
            assert_eq!(click(&mut state, button.x, button.y), None);
            let wheel = at(MouseEventKind::ScrollDown, 5, row);
            assert_eq!(handle_mouse(&mut state, wheel), None);
            assert_eq!(state.selected_node(), Some(&session("s0")));
            assert_eq!(state.tree.details, None);
        }

        // The details overlay swallows row clicks and the wheel too.
        let mut state = state_with(2);
        state.select_node(session("s0"));
        assert!(state.open_details());
        draw(&mut state, 80, 24);
        let row = screen_row(&state, index_of(&state, &session("s1")));
        assert_eq!(click(&mut state, 5, row), None);
        handle_mouse(&mut state, at(MouseEventKind::ScrollDown, 5, row));
        assert_eq!(state.selected_node(), Some(&session("s0")));
        assert_eq!(state.tree.details.as_deref(), Some("s0"));
    }

    #[test]
    fn click_focus_button_returns_focus() {
        let f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE);
        let mut state = state_with(1);
        state.select_node(session("s0"));
        draw(&mut state, 100, 24);

        // The action bar's button: same result as the key, and no details.
        let button = state
            .tree
            .hit
            .bar_focus
            .expect("a session shows the button");
        let expected = handle_key(&mut state.clone(), f);
        assert!(matches!(expected, Some(PendingAction::Focus(_))));
        assert_eq!(click(&mut state, button.x, button.y), expected);
        assert_eq!(state.tree.details, None);

        // The overlay's own button, likewise; the overlay stays open.
        assert!(state.open_details());
        draw(&mut state, 100, 24);
        let button = state.tree.hit.details_focus.expect("the overlay's button");
        let expected = handle_key(&mut state.clone(), f);
        assert!(matches!(expected, Some(PendingAction::Focus(_))));
        assert_eq!(click(&mut state, button.x, button.y), expected);
        assert_eq!(state.tree.details.as_deref(), Some("s0"));

        // Elsewhere in the overlay, a click is nothing.
        let overlay = state.tree.hit.details.expect("the overlay");
        assert_eq!(click(&mut state, overlay.x, overlay.y), None);

        // An owner or project has no button, as `f` has nothing to focus.
        let mut state = state_with(1);
        draw(&mut state, 100, 24);
        assert_eq!(state.tree.hit.bar_focus, None);
    }

    /// A short terminal puts the bar under the overlay: the bar's button is
    /// then under a modal and must not fire.
    #[test]
    fn bar_button_under_the_overlay_is_ignored() {
        let mut state = state_with(1);
        state.select_node(session("s0"));
        assert!(state.open_details());
        // Narrower and shorter than the overlay, which then fills the frame.
        draw(&mut state, 60, 13);
        let button = state.tree.hit.bar_focus.expect("the bar still records it");
        let overlay = state.tree.hit.details.expect("the overlay");
        assert!(overlay.contains(Position::new(button.x, button.y)));
        assert_eq!(click(&mut state, button.x, button.y), None);
    }

    #[test]
    fn wheel_moves_selection() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let row = state.tree.hit.tree_rows.y;
        assert_eq!(state.selected_node(), Some(&owner()));
        handle_mouse(&mut state, at(MouseEventKind::ScrollDown, 5, row));
        assert_eq!(state.selected_node(), Some(&project("alpha")));
        handle_mouse(&mut state, at(MouseEventKind::ScrollUp, 5, row));
        assert_eq!(state.selected_node(), Some(&owner()));
        assert!(
            state.tree.expanded.contains(&owner()),
            "the wheel only moves"
        );

        // Over the Activity strip the wheel does nothing.
        let below = state.tree.hit.tree_rows.bottom() + 2;
        handle_mouse(&mut state, at(MouseEventKind::ScrollDown, 5, below));
        assert_eq!(state.selected_node(), Some(&owner()));
    }

    #[test]
    fn button_release_and_other_buttons_ignored() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let row = screen_row(&state, index_of(&state, &project("alpha")));
        for kind in [
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Moved,
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            assert_eq!(handle_mouse(&mut state, at(kind, 5, row)), None);
        }
        assert_eq!(state.selected_node(), Some(&owner()));
        assert!(state.tree.expanded.contains(&project("alpha")));
    }

    /// The hit test uses the list's real offset: on a short terminal, with the
    /// selection scrolled far down, a click lands on the row drawn under it.
    #[test]
    fn click_hits_the_drawn_row_when_scrolled() {
        let mut state = state_with(30);
        state.select_node(session("s29"));
        let terminal = draw(&mut state, 80, 14);
        assert!(state.tree.nav.offset() > 0, "the tree must have scrolled");

        let rows = state.tree.hit.tree_rows;
        let screen = rows.y + 1;
        let line: String = (rows.x..rows.right())
            .map(|x| {
                terminal.backend().buffer()[(x, screen)]
                    .symbol()
                    .to_string()
            })
            .collect();
        let expected = state.tree.nav.offset() + 1;
        let NodeId::Session { id, .. } = state.tree.rows[expected].id.clone() else {
            panic!("row {expected} should be a session");
        };
        let number = id.trim_start_matches('s');
        assert!(line.contains(&format!("name-{number}")), "{line:?}");

        click(&mut state, rows.x + 2, screen);
        assert_eq!(state.tree.details.as_deref(), Some(id.as_str()));
    }
}
