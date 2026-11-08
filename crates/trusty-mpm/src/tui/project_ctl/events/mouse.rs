//! Mouse input for the projects view (#10476).
//!
//! Why: the owner asked that a click act like Enter and that a Focus button act
//! like `f` (#10476 comment 2). The handler stays pure, like `handle_key`, so
//! every branch is testable with synthetic events.
//! What: [`handle_mouse`] reads the rectangles the last frame recorded in
//! [`HitMap`] and routes one event. #10754: two-column MPM pane (Bob 11-06) —
//! a row click in either column selects the row and focuses its column
//! (`super::mouse_cols`). A click on a focus button sends an `f` key through
//! [`handle_key`], so the two cannot diverge. The wheel moves the selection
//! of the column under the cursor.
//! Everything else does nothing: a blank row, another pane, a button release,
//! and any event under a modal.
//! Test: `tests` below.
//!
//! [`HitMap`]: crate::tui::project_ctl::state::HitMap

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::{PendingAction, handle_key, mouse_cols};
use crate::tui::project_ctl::state::ProjectCtlState;

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

/// Is `(column, row)` on the action bar's live focus button?
fn on_focus_button(state: &ProjectCtlState, column: u16, row: u16) -> bool {
    state
        .tree
        .hit
        .bar_focus
        .is_some_and(|r| r.contains(Position::new(column, row)))
}

/// Route one mouse event, returning any action that needs a daemon call.
///
/// Why: see the module doc. The hit map describes the last drawn frame, and the
/// run loop draws between any two events, so a click always maps to what the
/// operator saw.
/// What: see the module doc.
/// Test: `click_owner_toggles`, `click_project_selects_and_focuses_tree`,
/// `click_session_toggles_details`, `click_blank_space_ignored`,
/// `click_outside_pane_ignored`, `click_under_modal_ignored`,
/// `click_focus_button_returns_focus`, `wheel_moves_selection`,
/// `click_hits_the_drawn_row_when_scrolled`.
pub fn handle_mouse(state: &mut ProjectCtlState, mouse: MouseEvent) -> Option<PendingAction> {
    if state.pending_confirm.is_some()
        || state.deliverable_view.is_some()
        || state.config_form.is_some()
        // #10754: De1 key map (Bob 11-06) — the rename modal is a modal too.
        || state.rename.is_some()
        // See #10754: the launch form too.
        || state.launch.is_some()
    {
        return None;
    }
    let (column, row) = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if on_focus_button(state, column, row) {
                return handle_key(state, KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
            }
            mouse_cols::click_row(state, column, row);
            None
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let down = mouse.kind == MouseEventKind::ScrollDown;
            mouse_cols::wheel(state, column, row, down);
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
    use crate::tui::project_ctl::state::rename::RenameModal;
    use crate::tui::project_ctl::state::{ConfirmKind, NodeId, Pane, ProjectRow, SessionRow};
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

    /// #10754: select session `id` in the right column (alpha on the left).
    fn select_session(state: &mut ProjectCtlState, id: &str) {
        assert!(state.focus_session(id), "{id} is listed");
    }

    fn selected_session_id(state: &ProjectCtlState) -> Option<String> {
        state.selected_session().map(|s| s.id.clone())
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
        state.tree.collapsed.remove(&project("alpha"));
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

    /// #10754: the screen cell showing session `id` in the right column.
    fn session_cell(state: &ProjectCtlState, id: &str) -> (u16, u16) {
        let col = &state.session_col;
        let index = col
            .rows
            .iter()
            .position(|r| r.session_id() == Some(id))
            .expect("a drawn session row");
        let rows = state.tree.hit.session_rows;
        (rows.x + 2, rows.y + (index - col.nav.offset()) as u16)
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
        assert!(state.tree.is_expanded(&owner()));
        assert_eq!(click(&mut state, 5, row), None);
        assert!(!state.tree.is_expanded(&owner()), "click closes");
        draw(&mut state, 80, 24);
        click(&mut state, 5, row);
        assert!(state.tree.is_expanded(&owner()), "click reopens");
    }

    /// #10754: a project is a leaf now; a click selects it and focuses the
    /// left column.
    #[test]
    fn click_project_selects_and_focuses_tree() {
        let mut state = state_with(2);
        state.focus = Pane::Activity;
        draw(&mut state, 80, 24);
        let row = screen_row(&state, index_of(&state, &project("alpha")));
        assert_eq!(click(&mut state, 5, row), None);
        assert_eq!(state.selected_node(), Some(&project("alpha")));
        assert_eq!(state.focus, Pane::Tree);
        assert!(state.session_col.expanded.is_empty());
    }

    /// #10754: a session row click expands it in place, as Right does (De1).
    #[test]
    fn click_session_toggles_details() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let (column, row) = session_cell(&state, "s1");
        assert_eq!(
            click(&mut state, column, row),
            None,
            "details need no daemon"
        );
        assert_eq!(selected_session_id(&state).as_deref(), Some("s1"));
        assert_eq!(state.focus, Pane::Sessions);
        assert!(state.session_col.expanded.contains("s1"));
    }

    #[test]
    fn click_blank_space_ignored() {
        let mut state = state_with(2);
        draw(&mut state, 80, 24);
        let before = state.selected_node().cloned();
        let expanded = state.tree.collapsed.clone();
        // Rows 0-1 hold the owner and the project; the next one is blank.
        let blank = screen_row(&state, state.tree.rows.len());
        assert_eq!(click(&mut state, 5, blank), None);
        assert_eq!(state.selected_node().cloned(), before);
        assert_eq!(state.tree.collapsed, expanded);
        assert!(state.session_col.expanded.is_empty());
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
        let open_modals: [fn(&mut ProjectCtlState); 5] = [
            |s| s.request_confirm(ConfirmKind::Kill, "s0", "name-0"),
            |s| s.open_deliverable_view("alpha"),
            |s| s.open_config_form("alpha"),
            // #10754: De1 key map (Bob 11-06)
            |s| s.rename = Some(RenameModal::prefilled("s0", "name-0")),
            // See #10754: the launch form.
            |s| {
                s.launch = Some(crate::tui::project_ctl::state::launch::LaunchModal::open(
                    "alpha",
                ))
            },
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
            select_session(&mut state, "s0");
            draw(&mut state, 80, 24);
            let (column, row) = session_cell(&state, "s1");
            let button = state.tree.hit.bar_focus.expect("the bar's button");
            open(&mut state);
            assert!(
                state.pending_confirm.is_some()
                    || state.deliverable_view.is_some()
                    || state.config_form.is_some()
                    || state.rename.is_some()
                    || state.launch.is_some(),
                "the modal must really be open"
            );

            assert_eq!(click(&mut state, column, row), None);
            assert_eq!(click(&mut state, button.x, button.y), None);
            let wheel = at(MouseEventKind::ScrollDown, column, row);
            assert_eq!(handle_mouse(&mut state, wheel), None);
            assert_eq!(selected_session_id(&state).as_deref(), Some("s0"));
            assert!(state.session_col.expanded.is_empty());
        }
    }

    #[test]
    fn click_focus_button_returns_focus() {
        let f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE);
        let mut state = state_with(1);
        select_session(&mut state, "s0");
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
        assert!(state.session_col.expanded.is_empty());

        // #10754: with the session expanded the button still fires.
        assert!(state.toggle_session_details());
        draw(&mut state, 100, 24);
        let button = state.tree.hit.bar_focus.expect("the bar's button");
        assert_eq!(click(&mut state, button.x, button.y), expected);
        assert!(state.session_col.expanded.contains("s0"), "and stays open");

        // An owner or project has no button, as `f` has nothing to focus.
        let mut state = state_with(1);
        draw(&mut state, 100, 24);
        assert_eq!(state.tree.hit.bar_focus, None);
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
        assert!(state.tree.is_expanded(&owner()), "the wheel only moves");

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
        assert!(state.tree.is_expanded(&project("alpha")));
    }

    /// The hit test uses the list's real offset: on a short terminal, with the
    /// selection scrolled far down, a click lands on the row drawn under it.
    /// #10754: the session list is the right column now.
    #[test]
    fn click_hits_the_drawn_row_when_scrolled() {
        let mut state = state_with(30);
        select_session(&mut state, "s29");
        let terminal = draw(&mut state, 80, 14);
        let offset = state.session_col.nav.offset();
        assert!(offset > 0, "the session column must have scrolled");

        let rows = state.tree.hit.session_rows;
        let screen = rows.y + 1;
        let line: String = (rows.x..rows.right())
            .map(|x| {
                terminal.backend().buffer()[(x, screen)]
                    .symbol()
                    .to_string()
            })
            .collect();
        let expected = offset + 1;
        let Some(id) = state.session_col.rows[expected]
            .session_id()
            .map(str::to_string)
        else {
            panic!("row {expected} should be a session");
        };
        let number = id.trim_start_matches('s');
        assert!(line.contains(&format!("name-{number}")), "{line:?}");

        click(&mut state, rows.x + 2, screen);
        assert_eq!(selected_session_id(&state), Some(id.clone()));
        assert!(state.session_col.expanded.contains(&id));
    }
}
