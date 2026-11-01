//! Selection, expand/collapse and the details overlay for the tree pane
//! (#10476).
//!
//! Why: the poll replaces the project and session lists on every tick. The
//! selection and the expanded set must survive that, so they name nodes by
//! [`NodeId`], never by row index, and the rows are rebuilt after every poll
//! and every toggle. Kept apart from `state/mod.rs`, which is near its cap.
//! What: [`TreeView`] holds the rows, the selection, the expanded set, the
//! worker-indent toggle and the open details overlay. The
//! [`ProjectCtlState`] methods below move, toggle and clamp the selection.
//! Test: `selection_and_expanded_survive_repoll`,
//! `removed_selected_session_clamps_to_project`, `left_on_child_goes_to_parent`.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use ratatui::layout::Rect;

use super::tree::{NodeId, TreeRow, build_tree, owner_of_repo_url};
use super::{ProjectCtlState, SessionRow};
use crate::session_manager::ssh_host_alias::SshHostAliases;
use crate::tui::coordinator::nav::ListNav;

/// The tree pane's view state; lives on [`ProjectCtlState::tree`].
#[derive(Debug, Clone, Default)]
pub struct TreeView {
    /// The visible rows, rebuilt by [`ProjectCtlState::rebuild_tree`].
    pub rows: Vec<TreeRow>,
    /// The selected node. Kept while the tree is empty (a down daemon), so a
    /// recovered poll restores it.
    pub selected: Option<NodeId>,
    /// The owners and projects drawn open. Everything starts collapsed.
    pub expanded: BTreeSet<NodeId>,
    /// `true` after `g` turned worker indentation off (default: indented).
    pub flat_workers: bool,
    /// Highlight and scroll offset for the rendered list.
    pub nav: ListNav,
    /// The session id whose read-only details overlay is open.
    pub details: Option<String>,
    /// #10476: where the last frame drew what a click can hit.
    pub hit: HitMap,
}

/// Where the last frame drew the mouse targets (#10476).
///
/// Why: a click arrives as a screen cell, and only the renderer knows the
/// layout. Recording it per frame means the hit test never recomputes it.
/// What: [`crate::tui::project_ctl::layout::render`] clears this and each pane
/// fills its part, so a pane not drawn this frame has no target.
/// Test: `click_hits_the_drawn_row_when_scrolled`,
/// `click_focus_button_returns_focus`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HitMap {
    /// The tree list's rows: its area without the borders.
    pub tree_rows: Rect,
    /// The action bar's `[f] focus` label, when the bar shows it.
    pub bar_focus: Option<Rect>,
    /// The details overlay's whole area, when it is drawn.
    pub details: Option<Rect>,
    /// The details overlay's `[f] focus window` label, when it is drawn.
    pub details_focus: Option<Rect>,
}

/// The `~/.ssh/config` aliases the owner parser resolves, read once.
///
/// Why: an scp-like remote through an alias (`git@gh-work:acme/x`) names its
/// owner only after the alias resolves; reading the file every tick is waste.
/// Unit tests never read the operator's config (#7196), so they get the empty
/// table.
fn aliases() -> &'static SshHostAliases {
    static ALIASES: OnceLock<SshHostAliases> = OnceLock::new();
    ALIASES.get_or_init(|| {
        if cfg!(test) {
            SshHostAliases::empty()
        } else {
            SshHostAliases::for_current_user()
        }
    })
}

impl ProjectCtlState {
    /// The selected tree node, if any.
    pub fn selected_node(&self) -> Option<&NodeId> {
        self.tree.selected.as_ref()
    }

    /// Rebuild the rows from the polled lists, then clamp the selection.
    ///
    /// Why: the one path both the poller and every toggle use, so a repoll
    /// and a key press can never disagree about which rows exist.
    /// What: [`build_tree`] over the current lists and expanded set, then
    /// [`ProjectCtlState::clamp_selection`].
    /// Test: `selection_and_expanded_survive_repoll`.
    pub fn rebuild_tree(&mut self) {
        // #10476: owner/project/session rows replace the two flat lists.
        self.tree.rows = build_tree(
            &self.projects,
            &self.sessions_by_project,
            &self.tree.expanded,
            !self.tree.flat_workers,
            aliases(),
        );
        self.clamp_selection();
    }

    /// Keep the selection on a visible row.
    ///
    /// Why: a selected session can be decommissioned, and a project can leave
    /// the registry, between two polls. The operator should land on the
    /// nearest surviving ancestor, not on an unrelated row.
    /// What: an empty tree keeps the selection untouched. Otherwise the
    /// selection walks [`NodeId::parent`] until it names a visible row; when
    /// nothing in its chain is visible, the first row is selected.
    /// Test: `removed_selected_session_clamps_to_project`,
    /// `empty_tree_keeps_the_selection_for_recovery`.
    fn clamp_selection(&mut self) {
        if self.tree.rows.is_empty() {
            self.tree.nav.sync_len(0);
            return;
        }
        let mut candidate = self.tree.selected.clone();
        while let Some(id) = &candidate {
            if self.row_index(id).is_some() {
                break;
            }
            candidate = id.parent();
        }
        let chosen = candidate.unwrap_or_else(|| self.tree.rows[0].id.clone());
        self.set_selected(chosen);
    }

    /// Index of `id` among the visible rows.
    fn row_index(&self, id: &NodeId) -> Option<usize> {
        self.tree.rows.iter().position(|r| &r.id == id)
    }

    /// Select `id` (which must be visible) and sync the highlight.
    ///
    /// A move to another project drops the Deliverables of the old one, as
    /// the Projects pane did (#2383): they would mark the new project's rows.
    fn set_selected(&mut self, id: NodeId) {
        let before = self.selected_project_name().map(str::to_string);
        self.tree.selected = Some(id);
        if self.selected_project_name() != before.as_deref() {
            self.deliverables = None;
        }
        let index = self
            .tree
            .selected
            .as_ref()
            .and_then(|id| self.row_index(id))
            .unwrap_or(0);
        self.tree.nav.sync_len(self.tree.rows.len());
        self.tree.nav.select(index);
    }

    /// Select the visible row at `index`; `false` when there is no such row.
    ///
    /// Test: `click_blank_space_ignored`, `click_session_opens_details`.
    pub(crate) fn select_row(&mut self, index: usize) -> bool {
        // #10476: a click names a row by position; the selection stays a NodeId.
        match self.tree.rows.get(index).map(|row| row.id.clone()) {
            Some(id) => {
                self.set_selected(id);
                true
            }
            None => false,
        }
    }

    /// Select `id`, opening its owner and project first so the row is visible.
    ///
    /// An `id` the tree does not hold leaves the selection where it was.
    /// Test: `select_node_expands_ancestors`.
    pub fn select_node(&mut self, id: NodeId) {
        let mut ancestor = id.parent();
        while let Some(node) = ancestor {
            ancestor = node.parent();
            self.tree.expanded.insert(node);
        }
        self.rebuild_tree();
        if self.row_index(&id).is_some() {
            self.set_selected(id);
        }
    }

    /// The owner of a registered project, as the tree derives it.
    pub(crate) fn owner_of_project(&self, project: &str) -> Option<String> {
        self.projects
            .iter()
            .find(|p| p.name == project)
            .map(|p| owner_of_repo_url(&p.repo_url, aliases()))
    }

    /// Move the selection one visible row down (`down`) or up, saturating.
    pub fn tree_step(&mut self, down: bool) {
        let Some(current) = self
            .tree
            .selected
            .as_ref()
            .and_then(|id| self.row_index(id))
        else {
            if let Some(first) = self.tree.rows.first() {
                self.set_selected(first.id.clone());
            }
            return;
        };
        let next = if down {
            (current + 1).min(self.tree.rows.len().saturating_sub(1))
        } else {
            current.saturating_sub(1)
        };
        self.set_selected(self.tree.rows[next].id.clone());
    }

    /// Left: collapse an open owner or project, else go to the parent node.
    ///
    /// Test: `left_on_child_goes_to_parent`, `left_collapses_then_right_expands`.
    pub fn tree_left(&mut self) {
        let Some(id) = self.tree.selected.clone() else {
            return;
        };
        if id.is_expandable() && self.tree.expanded.remove(&id) {
            self.rebuild_tree();
        } else if let Some(parent) = id.parent() {
            self.set_selected(parent);
        }
    }

    /// Right: expand a closed owner or project, else step into its first child.
    pub fn tree_right(&mut self) {
        let Some(id) = self.tree.selected.clone() else {
            return;
        };
        if !id.is_expandable() {
            return;
        }
        if self.tree.expanded.insert(id.clone()) {
            self.rebuild_tree();
        } else if let Some(i) = self.row_index(&id)
            && let Some(child) = self.tree.rows.get(i + 1)
            && child.id.parent().as_ref() == Some(&id)
        {
            self.set_selected(child.id.clone());
        }
    }

    /// Enter on an owner or project: flip it open or closed.
    pub fn toggle_selected(&mut self) {
        let Some(id) = self.tree.selected.clone() else {
            return;
        };
        if !id.is_expandable() {
            return;
        }
        if !self.tree.expanded.remove(&id) {
            self.tree.expanded.insert(id);
        }
        self.rebuild_tree();
    }

    /// `g`: turn worker indentation off or back on.
    pub fn toggle_worker_indent(&mut self) {
        self.tree.flat_workers = !self.tree.flat_workers;
        self.rebuild_tree();
        let mode = if self.tree.flat_workers { "off" } else { "on" };
        self.set_notice(format!("worker indentation {mode}"));
    }

    /// Open the read-only details overlay for the selected session.
    ///
    /// Returns `false` (and opens nothing) when the selection is not a session.
    pub fn open_details(&mut self) -> bool {
        match self.selected_session().map(|s| s.id.clone()) {
            Some(id) => {
                self.tree.details = Some(id);
                true
            }
            None => false,
        }
    }

    /// Close the details overlay.
    pub fn close_details(&mut self) {
        self.tree.details = None;
    }

    /// The session the open details overlay shows, with its project name.
    ///
    /// `None` when no overlay is open or the session left the fleet.
    pub fn details_session(&self) -> Option<(&str, &SessionRow)> {
        let id = self.tree.details.as_deref()?;
        self.sessions_by_project.iter().find_map(|(project, rows)| {
            rows.iter()
                .find(|s| s.id == id)
                .map(|s| (project.as_str(), s))
        })
    }
}

#[cfg(test)]
#[path = "tree_nav_tests.rs"]
mod tests;
