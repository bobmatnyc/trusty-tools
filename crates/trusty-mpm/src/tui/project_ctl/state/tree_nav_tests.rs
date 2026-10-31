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

fn session_node(id: &str) -> NodeId {
    NodeId::Session {
        owner: "acme".to_string(),
        project: "alpha".to_string(),
        id: id.to_string(),
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
fn first_poll_selects_the_first_owner_bar_collapsed() {
    let state = polled(&["s1"]);
    assert_eq!(state.selected_node(), Some(&acme()));
    assert_eq!(state.tree.rows.len(), 1, "owners start collapsed");
}

#[test]
fn selection_and_expanded_survive_repoll() {
    let mut state = polled(&["s1", "s2"]);
    state.select_node(session_node("s2"));
    let expanded = state.tree.expanded.clone();
    assert!(expanded.contains(&acme()) && expanded.contains(&alpha()));

    apply_core(&mut state, fleet(&["s0", "s1", "s2"]));
    apply_core(&mut state, fleet(&["s0", "s1", "s2"]));

    assert_eq!(state.selected_node(), Some(&session_node("s2")));
    assert_eq!(state.tree.expanded, expanded);
    let row = state.tree.nav.selected();
    assert_eq!(state.tree.rows[row].id, session_node("s2"));
}

#[test]
fn removed_selected_session_clamps_to_project() {
    let mut state = polled(&["s1", "s2"]);
    state.select_node(session_node("s2"));
    apply_core(&mut state, fleet(&["s1"]));
    assert_eq!(state.selected_node(), Some(&alpha()));
    assert!(state.selected_session().is_none());
}

#[test]
fn empty_tree_keeps_the_selection_for_recovery() {
    let mut state = polled(&["s1"]);
    state.select_node(session_node("s1"));
    apply_core(&mut state, CoreFetch::default());
    assert!(state.tree.rows.is_empty());
    apply_core(&mut state, fleet(&["s1"]));
    assert_eq!(state.selected_node(), Some(&session_node("s1")));
}

#[test]
fn select_node_expands_ancestors() {
    let mut state = polled(&["s1"]);
    state.select_node(session_node("s1"));
    assert!(state.tree.expanded.contains(&acme()));
    assert!(state.tree.expanded.contains(&alpha()));
    assert_eq!(state.selected_session().map(|s| s.id.as_str()), Some("s1"));
}

#[test]
fn left_on_child_goes_to_parent() {
    let mut state = polled(&["s1"]);
    state.select_node(session_node("s1"));
    state.tree_left();
    assert_eq!(state.selected_node(), Some(&alpha()));
    // An open project collapses first, then Left climbs to the owner.
    state.tree_left();
    assert_eq!(state.selected_node(), Some(&alpha()));
    assert!(!state.tree.expanded.contains(&alpha()));
    state.tree_left();
    assert_eq!(state.selected_node(), Some(&acme()));
}

#[test]
fn left_collapses_then_right_expands() {
    let mut state = polled(&["s1"]);
    state.tree_right();
    assert!(state.tree.expanded.contains(&acme()));
    assert_eq!(state.tree.rows.len(), 3, "acme, alpha, beta");
    // Right on an open owner steps into its first project.
    state.tree_right();
    assert_eq!(state.selected_node(), Some(&alpha()));
    state.tree_left();
    assert_eq!(state.selected_node(), Some(&acme()));
    state.tree_left();
    assert_eq!(state.tree.rows.len(), 1);
}

#[test]
fn step_saturates_at_both_ends() {
    let mut state = polled(&["s1"]);
    state.select_node(session_node("s1"));
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
