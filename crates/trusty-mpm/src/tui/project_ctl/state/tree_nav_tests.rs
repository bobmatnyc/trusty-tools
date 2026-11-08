//! Tests for tree selection and expand/collapse (#10476). The repoll tests go
//! through `poll::tick::apply_core`, the path the poller uses.

use std::collections::BTreeMap;

use super::super::{ProjectRow, SessionRow};
use super::*;
use crate::tui::project_ctl::poll::tick::{CoreFetch, apply_core};

fn project(name: &str) -> ProjectRow {
    ProjectRow {
        name: name.to_string(),
        repo_url: format!("https://github.com/acme/{name}"),
        ..Default::default()
    }
}

fn session(id: &str) -> SessionRow {
    SessionRow {
        id: id.to_string(),
        name: format!("session-{id}"),
        state: "active".to_string(),
        ..Default::default()
    }
}

fn acme() -> NodeId {
    NodeId::Owner("acme".to_string())
}

fn alpha() -> NodeId {
    NodeId::Project {
        owner: "acme".to_string(),
        name: "alpha".to_string(),
    }
}

fn beta() -> NodeId {
    NodeId::Project {
        owner: "acme".to_string(),
        name: "beta".to_string(),
    }
}

/// A fleet with project alpha holding `ids`, and an empty project beta.
fn fleet(ids: &[&str]) -> CoreFetch {
    let mut sessions = BTreeMap::new();
    sessions.insert(
        "alpha".to_string(),
        ids.iter().map(|id| session(id)).collect::<Vec<_>>(),
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

fn polled(ids: &[&str]) -> ProjectCtlState {
    let mut state = ProjectCtlState::default();
    apply_core(&mut state, fleet(ids));
    state
}

#[test]
fn first_poll_selects_the_first_owner_bar_expanded() {
    // #10476: tree starts expanded (Bob 11-05 17:30Z)
    let state = polled(&["s1"]);
    assert_eq!(state.selected_node(), Some(&acme()));
    // #10754: two-column MPM pane (Bob 11-06) — no session rows on the left.
    assert_eq!(state.tree.rows.len(), 3, "acme, alpha, beta all open");
}

#[test]
fn selection_and_expanded_survive_repoll() {
    let mut state = polled(&["s1", "s2"]);
    state.select_node(alpha());
    let expanded = state.tree.collapsed.clone();
    assert!(state.tree.is_expanded(&acme()));

    apply_core(&mut state, fleet(&["s0", "s1", "s2"]));
    apply_core(&mut state, fleet(&["s0", "s1", "s2"]));

    assert_eq!(state.selected_node(), Some(&alpha()));
    assert_eq!(state.tree.collapsed, expanded);
    let row = state.tree.nav.selected();
    assert_eq!(state.tree.rows[row].id, alpha());
}

/// #10754: the left column has no session rows, so the node a repoll can
/// remove is a project; the selection climbs to its owner.
#[test]
fn removed_selected_project_clamps_to_owner() {
    let mut state = polled(&["s1"]);
    state.select_node(beta());
    let mut gone = fleet(&["s1"]);
    if let Some((projects, _, _)) = gone.fleet.as_mut() {
        projects.retain(|p| p.name != "beta");
    }
    apply_core(&mut state, gone);
    assert_eq!(state.selected_node(), Some(&acme()));
    assert!(state.selected_project().is_none());
}

#[test]
fn empty_tree_keeps_the_selection_for_recovery() {
    let mut state = polled(&["s1"]);
    assert!(state.focus_session("s1"));
    apply_core(&mut state, CoreFetch::default());
    assert!(state.tree.rows.is_empty());
    assert!(state.session_col.rows.is_empty());
    apply_core(&mut state, fleet(&["s1"]));
    assert_eq!(state.selected_node(), Some(&alpha()));
    assert_eq!(state.selected_session().map(|s| s.id.as_str()), Some("s1"));
}

#[test]
fn select_node_expands_ancestors() {
    let mut state = polled(&["s1"]);
    state.tree.collapsed.insert(acme());
    state.rebuild_tree();
    assert_eq!(state.tree.rows.len(), 1, "the owner is closed");
    state.select_node(alpha());
    assert!(state.tree.is_expanded(&acme()));
    assert_eq!(
        state.selected_project().map(|p| p.name.as_str()),
        Some("alpha")
    );
}

#[test]
fn left_on_child_goes_to_parent() {
    let mut state = polled(&["s1"]);
    state.select_node(alpha());
    state.tree_left();
    assert_eq!(state.selected_node(), Some(&acme()));
    // An open owner collapses on the next Left.
    state.tree_left();
    assert_eq!(state.selected_node(), Some(&acme()));
    assert!(!state.tree.is_expanded(&acme()));
}

#[test]
fn left_collapses_then_right_expands() {
    let mut state = polled(&["s1"]);
    state.tree_left();
    assert!(!state.tree.is_expanded(&acme()));
    assert_eq!(state.tree.rows.len(), 1);
    state.tree_right();
    assert!(state.tree.is_expanded(&acme()));
    assert_eq!(state.tree.rows.len(), 3, "acme, alpha, beta");
    // Right on an open owner steps into its first project.
    state.tree_right();
    assert_eq!(state.selected_node(), Some(&alpha()));
    // #10754: Right on a project (a leaf) enters the session column.
    state.tree_right();
    assert_eq!(state.focus, super::super::Pane::Sessions);
    assert_eq!(state.selected_node(), Some(&alpha()));
    // Left on the project climbs to the owner, then closes it.
    state.tree_left();
    assert_eq!(state.selected_node(), Some(&acme()));
    state.tree_left();
    assert_eq!(state.tree.rows.len(), 1);
}

#[test]
fn step_saturates_at_both_ends() {
    let mut state = polled(&["s1"]);
    state.select_node(alpha());
    state.tree_step(true);
    state.tree_step(true);
    state.tree_step(true);
    let last = state.tree.rows.last().unwrap().id.clone();
    assert_eq!(state.selected_node(), Some(&last));
    for _ in 0..10 {
        state.tree_step(false);
    }
    assert_eq!(state.selected_node(), Some(&acme()));
}

#[test]
fn toggle_worker_indent_rebuilds_and_says_so() {
    let mut state = polled(&["s1"]);
    state.toggle_worker_indent();
    assert!(state.tree.flat_workers);
    assert_eq!(state.notice.as_deref(), Some("worker indentation off"));
    state.toggle_worker_indent();
    assert!(!state.tree.flat_workers);
}
