//! All mutable state for the search TUI, with navigation methods.

use crate::monitor::dashboard::IndexRow;
use crate::monitor::tui_common::{ListFocus, ThreeWaySortKey};
use crate::monitor::utils::{ActivityLog, DaemonStatus};

use super::nav::visible_index_ids;

/// Which zone of the search UI currently holds keyboard focus.
///
/// Why: re-export alias for [`ListFocus`] so existing callers and tests that
/// reference `SearchFocus` continue to compile after the type was consolidated
/// into the shared module.
/// What: type alias for [`ListFocus`].
/// Test: `test_toggle_focus`.
pub type SearchFocus = ListFocus;

/// All mutable state the search UI renders and mutates.
///
/// Why: the event loop polls the daemon, streams reindex events, and handles
/// input — keeping every piece of state in one struct keeps the loop terse and
/// the rendering a pure function of this snapshot.
/// What: the daemon URL and status, the index list and selection cursor, the
/// scroll offset of the index panel, the bounded activity log, the query
/// buffer, the focused zone, and the help flag. The selection cursor addresses
/// a list whose first row is the synthetic "All indexes" entry, so cursor `0`
/// means "All" and cursor `n` (n ≥ 1) means `indexes[n - 1]`.
/// Test: `test_selected_clamp`, `test_toggle_focus`, `test_log_append`,
/// `test_all_selector`, `test_scroll_offset`.
#[derive(Debug, Clone)]
pub struct SearchTuiState {
    /// The trusty-search daemon base URL being monitored.
    pub base_url: String,
    /// The daemon's current liveness state.
    pub daemon_status: DaemonStatus,
    /// One row per registered index.
    pub indexes: Vec<IndexRow>,
    /// Cursor into the index list, where row `0` is the "All indexes" entry
    /// and row `n` (n ≥ 1) selects `indexes[n - 1]`.
    pub selected: usize,
    /// Index of the first row drawn in the INDEXES panel — the scroll offset
    /// that keeps [`Self::selected`] on screen when the list overflows.
    pub scroll_offset: usize,
    /// Bounded, timestamped log of reindex / search activity.
    pub log: ActivityLog,
    /// The in-progress search query buffer.
    pub input: String,
    /// Which zone currently holds keyboard focus.
    pub focus: SearchFocus,
    /// Whether the help overlay is visible (toggled with `?`).
    pub show_help: bool,
    /// Case-insensitive filter applied to index id / project; empty disables.
    pub filter: String,
    /// Whether the inline filter bar is focused (captures typed chars).
    pub filter_active: bool,
    /// Current index-list sort order.
    pub sort_key: ThreeWaySortKey,
    /// Whether the index list is grouped by inferred project.
    pub group_by_project: bool,
    /// The last daemon log line seen on the previous `logs_tail` poll.
    ///
    /// Why: `GET /logs/tail` returns the last N lines from a ring buffer; the
    /// poll cycle uses this watermark to identify only the lines that arrived
    /// since the previous tick, so historical lines are not re-pushed every
    /// 2 seconds.
    /// What: the most-recent line from the previous successful poll, or
    /// `None` before the first poll completes.
    /// Test: `test_push_new_log_lines_skips_first_poll`.
    pub log_watermark: Option<String>,
    /// True until the first successful `logs_tail` response.
    ///
    /// Why: when the operator opens the TUI, the ring buffer may already
    /// hold lines from earlier daemon activity; dumping them all into the
    /// activity feed would bury whatever happens next. This flag suppresses
    /// the initial dump and only records a watermark on the first poll.
    /// What: starts `true`, flips to `false` after the first poll resolves
    /// the watermark.
    /// Test: `test_push_new_log_lines_skips_first_poll`.
    pub log_first_poll: bool,
}

impl SearchTuiState {
    /// Build a fresh search UI state targeting `base_url`.
    ///
    /// Why: the event loop seeds the state at startup before the first poll.
    /// What: stores the URL, sets the daemon `Connecting`, and starts with an
    /// empty index list, empty log, empty query, and list focus.
    /// Test: `test_new_state_defaults`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            daemon_status: DaemonStatus::Connecting,
            indexes: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            log: ActivityLog::new(),
            input: String::new(),
            focus: ListFocus::List,
            show_help: false,
            filter: String::new(),
            filter_active: false,
            sort_key: ThreeWaySortKey::default(),
            group_by_project: false,
            log_watermark: None,
            log_first_poll: true,
        }
    }

    /// Cycle keyboard focus between the index list and the query bar (`[Tab]`).
    ///
    /// Why: `[Tab]` decides whether arrows navigate the list or whether typed
    /// characters edit the search query.
    /// What: flips [`Self::focus`] via [`ListFocus::toggled`].
    /// Test: `test_toggle_focus`.
    pub fn toggle_focus(&mut self) {
        self.focus = self.focus.toggled();
    }

    /// Move the index selection up one row, saturating at the top.
    ///
    /// Why: `↑` navigates the INDEXES list when it has focus.
    /// What: decrements [`Self::selected`], never below zero.
    /// Test: `test_selected_clamp`.
    pub fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the index selection down one row, clamped to the last index.
    ///
    /// Why: `↓` navigates the INDEXES list when it has focus.
    /// What: increments [`Self::selected`] but never past the last row. The
    /// list has `indexes.len() + 1` rows (row 0 is "All indexes").
    /// Test: `test_selected_clamp`.
    pub fn select_down(&mut self) {
        if self.selected < self.last_row() {
            self.selected += 1;
        }
    }

    /// The index of the last selectable row.
    ///
    /// Why: the list always carries the synthetic "All" row, so the last valid
    /// cursor is `indexes.len()` (not `indexes.len() - 1`).
    /// What: returns `indexes.len()` — row 0 is "All", rows `1..=len` are the
    /// individual indexes.
    /// Test: `test_selected_clamp`.
    pub fn last_row(&self) -> usize {
        self.indexes.len()
    }

    /// Clamp the selection cursor to the current index count.
    ///
    /// Why: a poll can shrink the index list (an index was deleted) leaving the
    /// cursor past the end; this keeps it valid before rendering.
    /// What: caps [`Self::selected`] at `indexes.len()` (the "All" row plus one
    /// row per index).
    /// Test: `test_selected_clamp`.
    pub fn clamp_selection(&mut self) {
        if self.selected > self.last_row() {
            self.selected = self.last_row();
        }
    }

    /// Recompute the scroll offset so the selected row fits a `visible` window.
    ///
    /// Why: the INDEXES panel is a fixed-height viewport; when the list has
    /// more rows than fit, the panel must scroll so [`Self::selected`] is never
    /// drawn off-screen — otherwise `↑`/`↓` appear to do nothing past the edge.
    /// What: given the panel's visible row count, shifts [`Self::scroll_offset`]
    /// down when the cursor falls below the window and up when it rises above
    /// it, leaving it untouched while the cursor is already in view. A zero
    /// `visible` is treated as one row so the offset always tracks the cursor.
    /// Test: `test_scroll_offset`.
    pub fn sync_scroll(&mut self, visible: usize) {
        let cursor = self.selected;
        self.sync_scroll_to(cursor, visible);
    }

    /// Recompute the scroll offset for an arbitrary cursor row.
    ///
    /// Why: when filtering, sorting, or grouping reorders the rendered rows,
    /// `Self::selected` (an index into the original `indexes` array) no
    /// longer matches the row's on-screen position. The renderer must pass
    /// in the *visible* row index so the viewport scrolls to the row the
    /// user actually sees as selected.
    /// What: identical scroll math to [`Self::sync_scroll`] but anchored on
    /// the supplied `cursor_row` instead of `self.selected`.
    /// Test: `test_sync_scroll_to_follows_sorted_order`.
    pub fn sync_scroll_to(&mut self, cursor_row: usize, visible: usize) {
        let window = visible.max(1);
        if cursor_row >= self.scroll_offset + window {
            self.scroll_offset = cursor_row + 1 - window;
        } else if cursor_row < self.scroll_offset {
            self.scroll_offset = cursor_row;
        }
    }

    /// Whether the "All indexes" entry is currently selected.
    ///
    /// Why: when "All" is selected the UI fans queries out across every index
    /// and aggregates the activity feed and statistics.
    /// What: returns `true` exactly when the cursor is on row 0.
    /// Test: `test_all_selector`.
    pub fn is_all_selected(&self) -> bool {
        self.selected == 0
    }

    /// The id of the currently selected single index, if any.
    ///
    /// Why: `[r]` reindexes and `[Enter]` searches a single index; both need
    /// its id, and neither applies when "All" is selected.
    /// What: returns `Some(id)` for the index at cursor row `n ≥ 1`, or `None`
    /// when "All" is selected or the index list is empty.
    /// Test: `test_selected_id`.
    pub fn selected_id(&self) -> Option<&str> {
        if self.selected == 0 {
            return None;
        }
        self.indexes.get(self.selected - 1).map(|i| i.id.as_str())
    }

    /// Clamp the selection to the currently visible (filtered + sorted) list.
    ///
    /// Why: when the filter changes the selected index may no longer appear in
    /// the visible subset, so arrow navigation would jump unpredictably; this
    /// drops the cursor back to "All" (row 0) in that case so navigation always
    /// starts from a visible row.
    /// What: if `selected` is non-zero and the corresponding index id is not in
    /// the visible id list, resets `selected` to 0.
    /// Test: `test_clamp_to_visible`.
    pub fn clamp_to_visible(&mut self) {
        if self.selected == 0 {
            return;
        }
        let Some(current_id) = self.indexes.get(self.selected - 1).map(|i| i.id.clone()) else {
            self.selected = 0;
            return;
        };
        let ids = visible_index_ids(self);
        if !ids.iter().any(|id| id == &current_id) {
            self.selected = 0;
        }
    }

    /// The scope filter for the activity feed and statistics panels.
    ///
    /// Why: the right-hand panels render the selected index's events / stats,
    /// or every index's when "All" is selected; this folds the cursor into the
    /// `Option<&str>` filter [`ActivityLog::tail_scoped`] expects.
    /// What: returns `None` when "All" is selected (un-filtered) or `Some(id)`
    /// for the selected single index.
    /// Test: `test_all_selector`.
    pub fn scope_filter(&self) -> Option<&str> {
        self.selected_id()
    }
}
