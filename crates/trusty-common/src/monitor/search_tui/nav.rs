//! Navigation helpers: filter/sort, visible id list, and cursor movement.

use crate::monitor::dashboard::IndexRow;
use crate::monitor::tui_common;

use super::state::SearchTuiState;

/// Apply [`SearchTuiState::filter`] and [`SearchTuiState::sort_key`] to the
/// state's indexes, returning the visible subset in display order.
///
/// Why: delegates to the shared [`tui_common::filtered_sorted`] so memory and
/// search apply identical filter / sort rules. Kept as a search-named wrapper
/// for the existing tests and callers.
/// What: thin wrapper over [`tui_common::filtered_sorted`].
/// Test: `test_apply_filter`, `test_apply_sort_*`.
pub fn filtered_sorted_indexes(state: &SearchTuiState) -> Vec<IndexRow> {
    tui_common::filtered_sorted(&state.indexes, &state.filter, state.sort_key)
}

/// Ids of the rows the user can navigate between, in visible display order.
///
/// Why: thin wrapper over the shared [`tui_common::visible_ids`].
/// What: delegates to the shared helper with the search state's fields.
/// Test: `test_visible_index_ids`, `test_navigate_visible`.
pub fn visible_index_ids(state: &SearchTuiState) -> Vec<String> {
    tui_common::visible_ids(
        &state.indexes,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    )
}

/// Move the cursor up one row in the visible (filtered + sorted) list.
///
/// Why: thin wrapper over the shared [`tui_common::navigate_up`].
/// What: delegates and writes back the new cursor.
/// Test: `test_navigate_visible`.
pub fn navigate_up_visible(state: &mut SearchTuiState) {
    state.selected = tui_common::navigate_up(
        &state.indexes,
        state.selected,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    );
}

/// Move the cursor down one row in the visible (filtered + sorted) list.
///
/// Why: thin wrapper over the shared [`tui_common::navigate_down`].
/// What: delegates and writes back the new cursor.
/// Test: `test_navigate_visible`.
pub fn navigate_down_visible(state: &mut SearchTuiState) {
    state.selected = tui_common::navigate_down(
        &state.indexes,
        state.selected,
        &state.filter,
        state.sort_key,
        state.group_by_project,
    );
}

/// Row index — within the rendered `index_lines` output — that the cursor
/// currently sits on.
///
/// Why: ratatui's `ListState::with_selected` and the viewport scroll math
/// both index into the rendered list, but `state.selected` is an index into
/// the *original* `state.indexes` Vec. After a filter, sort, or grouping
/// reorders rows, the two indices diverge and the highlight + scroll latch
/// onto the wrong on-screen line. This helper bridges them: given the same
/// state the renderer sees, it returns the visible row at which the current
/// selection is drawn so the highlight follows the sorted order.
/// What: returns `0` when "All" is selected; otherwise walks
/// [`index_lines`] looking for the row whose `selected` flag is set and
/// returns its index. Falls back to `0` (the "All" row) when no matching
/// row is found, which mirrors how `clamp_to_visible` collapses a hidden
/// selection back to "All".
/// Test: `test_visible_selected_row_follows_sort`,
/// `test_visible_selected_row_follows_group`.
pub fn visible_selected_row(state: &SearchTuiState) -> usize {
    if state.selected == 0 {
        return 0;
    }
    super::render::index_lines(state)
        .iter()
        .position(|row| row.selected)
        .unwrap_or(0)
}

/// Compute which lines from `new_lines` are genuinely new since `watermark`.
///
/// Why: `GET /logs/tail` returns the last N lines from a ring buffer; on
/// every poll we receive an overlapping window. This helper finds the
/// suffix of `new_lines` that appears after the last-seen watermark line,
/// so only truly new lines get pushed to the activity log.
/// What: if `watermark` is `None`, returns all of `new_lines` (first poll
/// after the initial skip). Otherwise finds the rightmost occurrence of
/// `watermark` in `new_lines` and returns everything after it. If the
/// watermark is not found (ring buffer wrapped), returns all of `new_lines`.
/// Test: `test_new_log_lines_since_watermark`.
pub fn new_log_lines_since<'a>(new_lines: &'a [String], watermark: Option<&str>) -> &'a [String] {
    let Some(mark) = watermark else {
        return new_lines;
    };
    match new_lines.iter().rposition(|line| line == mark) {
        Some(idx) => &new_lines[idx + 1..],
        None => new_lines,
    }
}
