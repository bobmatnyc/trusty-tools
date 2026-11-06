//! The session column of the two-column MPM pane (#10754).
//!
//! Why: the owner asked for owner/project on the left and the sessions list on
//! the right (Bob 11-06 00:47Z). The right column is a pure projection of the
//! polled session lists and the left selection, so it is rebuilt with the
//! tree and keeps its own selection by session id across a repoll.
//! What: [`SessionColumn`] holds the rows, the selected session id and the
//! list highlight. [`build_session_rows`] lists a project's sessions with
//! worker indentation, or an owner's sessions grouped under project header
//! lines (Architect Q1). The [`ProjectCtlState`] methods below move, clamp and
//! read the selection, and move focus between the columns.
//! Test: `two_column_tests.rs` (`render_100x30_shows_both_columns`,
//! `owner_selected_groups_sessions_by_project`,
//! `session_selection_survives_repoll_and_clamps_when_it_vanishes`).

use std::collections::BTreeMap;

use super::tree::{NodeId, Placement, counters, session_placements};
use super::{Pane, ProjectCtlState, ProjectRow, SessionRow};
use crate::tui::coordinator::nav::ListNav;

/// One line of the session column, in draw order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionColRow {
    /// A project header, drawn only while an owner is selected (Q1).
    Header {
        project: String,
        total: usize,
        live: usize,
    },
    /// Session `id`, at `index` in `sessions_by_project[project]`, drawn at
    /// worker `depth`.
    Session {
        project: String,
        id: String,
        index: usize,
        depth: usize,
    },
    /// The dimmed line standing in for a parent the project does not list.
    Anchor { parent: String, depth: usize },
}

impl SessionColRow {
    /// The session id of a session row; `None` for a header or an anchor.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            SessionColRow::Session { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// The session column's view state; lives on [`ProjectCtlState::session_col`].
///
/// Why: each column keeps its own selection (#10754 S1). Naming the selection
/// by session id lets it survive a repoll that reorders the rows.
/// What: `selected` is `None` until the operator enters the column, so an
/// unvisited column asks the daemon for no `/activity`. `source` is the left
/// node the rows were built for; a change of it drops the selection.
/// Test: `session_selection_survives_repoll_and_clamps_when_it_vanishes`.
#[derive(Debug, Clone, Default)]
pub struct SessionColumn {
    /// The drawn rows.
    pub rows: Vec<SessionColRow>,
    /// The selected session id. Kept while the rows are empty (a down
    /// daemon), so a recovered poll restores it.
    pub selected: Option<String>,
    /// Highlight and scroll offset for the rendered list.
    pub nav: ListNav,
    /// The left node `rows` were built for.
    source: Option<NodeId>,
}

impl SessionColumn {
    /// Index of the session row for `id`.
    fn index_of(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.session_id() == Some(id))
    }

    /// Indices of the selectable (session) rows, in draw order.
    fn session_indices(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.session_id().is_some())
            .map(|(i, _)| i)
            .collect()
    }
}

/// The rows the session column draws for the left node `source`.
///
/// Why: one pure builder, so the pane, the keys and the clamp agree on the rows.
/// What: a project lists its sessions in [`session_placements`] order
/// (`indent` on) or flat in poll order. An owner lists the sessions of each
/// of its projects that has any, each group under a
/// [`SessionColRow::Header`], in registry order. `owner_of` maps a project
/// name to its owner key. No node, or no sessions, gives no rows.
/// Test: `render_100x30_shows_both_columns`,
/// `owner_selected_groups_sessions_by_project`.
pub(crate) fn build_session_rows(
    source: Option<&NodeId>,
    projects: &[ProjectRow],
    sessions_by_project: &BTreeMap<String, Vec<SessionRow>>,
    indent: bool,
    owner_of: impl Fn(&str) -> Option<String>,
) -> Vec<SessionColRow> {
    let mut rows = Vec::new();
    match source {
        Some(NodeId::Project { name, .. }) => {
            if let Some(sessions) = sessions_by_project.get(name) {
                push_project(&mut rows, name, sessions, indent);
            }
        }
        Some(NodeId::Owner(owner)) => {
            for project in projects {
                let Some(sessions) = sessions_by_project.get(&project.name) else {
                    continue;
                };
                if sessions.is_empty() || owner_of(&project.name).as_ref() != Some(owner) {
                    continue;
                }
                let (total, live) = counters(sessions);
                rows.push(SessionColRow::Header {
                    project: project.name.clone(),
                    total,
                    live,
                });
                push_project(&mut rows, &project.name, sessions, indent);
            }
        }
        None => {}
    }
    rows
}

/// Append one project's session rows (and anchors) to `rows`.
fn push_project(
    rows: &mut Vec<SessionColRow>,
    project: &str,
    sessions: &[SessionRow],
    indent: bool,
) {
    let placements = if indent {
        session_placements(sessions)
    } else {
        (0..sessions.len())
            .map(|i| Placement::Session(i, 0))
            .collect()
    };
    rows.extend(placements.into_iter().map(|placement| match placement {
        Placement::Session(index, depth) => SessionColRow::Session {
            project: project.to_string(),
            id: sessions[index].id.clone(),
            index,
            depth,
        },
        Placement::Anchor(parent) => SessionColRow::Anchor { parent, depth: 0 },
    }));
}

impl ProjectCtlState {
    /// The currently selected session row, if any.
    ///
    /// #10754: the right column's selection. `None` until the operator enters
    /// that column, and when the selected session is not in its rows.
    /// Test: `selected_session_is_none_on_owner_and_project_nodes`.
    pub fn selected_session(&self) -> Option<&SessionRow> {
        let id = self.session_col.selected.as_deref()?;
        let index = self.session_col.index_of(id)?;
        let SessionColRow::Session { project, index, .. } = &self.session_col.rows[index] else {
            return None;
        };
        self.sessions_by_project.get(project)?.get(*index)
    }

    /// Rebuild the session column for the current left selection, then clamp.
    ///
    /// Why: called by [`ProjectCtlState::rebuild_tree`] and on every left
    /// selection change, so the column always shows the selected node.
    /// What: a changed left node drops the selection. Empty rows keep the
    /// selection (a down daemon). A selected id that left the rows moves to
    /// the session row nearest its old position, or to none.
    /// Test: `session_selection_survives_repoll_and_clamps_when_it_vanishes`.
    pub(crate) fn rebuild_session_col(&mut self) {
        let source = self.tree.selected.clone();
        let rows = build_session_rows(
            source.as_ref(),
            &self.projects,
            &self.sessions_by_project,
            !self.tree.flat_workers,
            |name| self.owner_of_project(name),
        );
        let col = &mut self.session_col;
        if col.source != source {
            col.source = source;
            col.selected = None;
        }
        let old = col.nav.selected();
        col.rows = rows;
        col.nav.sync_len(col.rows.len());
        let Some(id) = col.selected.clone() else {
            return;
        };
        if col.rows.is_empty() {
            return;
        }
        let index = col.index_of(&id).or_else(|| {
            let indices = col.session_indices();
            let after = indices.iter().copied().find(|&i| i >= old);
            after.or_else(|| indices.last().copied())
        });
        self.select_session_index(index);
    }

    /// Select the session row at `index`, or clear the selection on `None`.
    fn select_session_index(&mut self, index: Option<usize>) {
        let col = &mut self.session_col;
        col.selected = index.and_then(|i| col.rows.get(i)?.session_id().map(str::to_string));
        if let Some(i) = index.filter(|_| col.selected.is_some()) {
            col.nav.select(i);
        }
    }

    /// Select the session row at `index`; `false` (no change) on any other row.
    ///
    /// Test: `click_selects_and_focuses_either_column`.
    pub(crate) fn select_session_row(&mut self, index: usize) -> bool {
        let is_session = self
            .session_col
            .rows
            .get(index)
            .is_some_and(|r| r.session_id().is_some());
        if is_session {
            self.select_session_index(Some(index));
        }
        is_session
    }

    /// Move the session selection to the next session row down or up,
    /// skipping headers and anchors and saturating at both ends. With nothing
    /// selected, select the first session.
    pub fn session_step(&mut self, down: bool) {
        let col = &self.session_col;
        let current = col.selected.as_deref().and_then(|id| col.index_of(id));
        let indices = col.session_indices();
        let mut it = indices.iter().copied();
        let next = match (current, down) {
            (None, _) => it.next(),
            (Some(c), true) => it.find(|&i| i > c).or(Some(c)),
            (Some(c), false) => it.filter(|&i| i < c).last().or(Some(c)),
        };
        self.select_session_index(next);
    }

    /// Focus the session column, selecting its first session when none is.
    ///
    /// Test: `tab_and_arrows_move_focus_between_columns`.
    pub fn focus_sessions(&mut self) {
        self.focus = Pane::Sessions;
        if self.selected_session().is_none() {
            self.session_step(true);
        }
    }

    /// Cycle focus forward (Tree → Sessions → Activity → …).
    pub fn cycle_focus_next(&mut self) {
        self.set_focus(self.focus.next());
    }

    /// Cycle focus backward.
    pub fn cycle_focus_prev(&mut self) {
        self.set_focus(self.focus.prev());
    }

    /// Focus `pane`; the session column gets a selection on entry.
    fn set_focus(&mut self, pane: Pane) {
        if pane == Pane::Sessions {
            self.focus_sessions();
        } else {
            self.focus = pane;
        }
    }

    /// Select the project of `session_id` on the left and the session on the
    /// right, and focus the session column.
    ///
    /// Why: #6483 routes `tm attach <target>` at this multipane dashboard.
    /// That command resolves a session id and expects the TUI to open ON that
    /// session; without this the operator would land on whichever owner sorts
    /// first and have to hunt for it.
    /// What: scans `projects` in registry order for the first project whose
    /// `sessions_by_project` entry contains `session_id`, selects that project
    /// node, then the session in the right column (#10754). Returns `false`
    /// (leaving every selection untouched) when the id is unknown — the fleet
    /// may not have polled it yet.
    /// Test: `focus_session_selects_owning_project_and_row`,
    /// `focus_session_is_a_noop_for_an_unknown_id`.
    pub fn focus_session(&mut self, session_id: &str) -> bool {
        let Some(project) = self
            .projects
            .iter()
            .find(|p| {
                self.sessions_by_project
                    .get(&p.name)
                    .is_some_and(|rows| rows.iter().any(|s| s.id == session_id))
            })
            .map(|p| p.name.clone())
        else {
            return false;
        };
        let Some(owner) = self.owner_of_project(&project) else {
            return false;
        };
        self.select_node(NodeId::Project {
            owner,
            name: project,
        });
        let index = self.session_col.index_of(session_id);
        self.select_session_index(index);
        self.focus = Pane::Sessions;
        true
    }
}
