//! Numbered scrollable list navigation for the coordinator TUI (STUI-1).
//!
//! Why: Child #2 (#1386) landed live polling but rendered the unified list with
//! a plain, non-scrolling `List` whose selection was a bare `usize` on
//! [`CoordinatorState`]. STUI-1 (#1278) needs a numbered, scrollable list that
//! shows at least five rows, scrolls with the arrow keys + Page-Up/Page-Down,
//! highlights the selected row, and — crucially — PRESERVES the selection and
//! the scroll offset across the timer re-polls Child #2 added (a refresh must not
//! yank the operator back to the top). ratatui's [`ListState`] already owns both
//! the `selected` index AND the `offset` and auto-scrolls the offset to keep the
//! selection visible, so the navigation model is a thin, pure wrapper over it
//! that the render path can hand straight to `render_stateful_widget`. Keeping it
//! in its own module (rather than swelling `state.rs`) preserves the
//! one-concept-per-file split and the SLOC cap.
//! What: [`ListNav`] wraps a [`ListState`] plus the current row count and offers
//! pure movers ([`ListNav::up`], [`ListNav::down`], [`ListNav::page_up`],
//! [`ListNav::page_down`]) that saturate at the list bounds, a
//! [`ListNav::sync_len`] that re-clamps the selection when the row count changes
//! between polls (preserving the offset where still valid), and the
//! [`ListNav::state_mut`] accessor the renderer passes to
//! `render_stateful_widget`. [`MIN_VISIBLE_ROWS`] documents the spec's
//! five-row-minimum viewport.
//! Test: `super::tests` covers up/down saturation, page scrolling, selection +
//! offset preservation across `sync_len`, and clamp-on-shrink.

use ratatui::widgets::ListState;

/// Minimum number of session rows the list viewport must show (STUI-1 §AC).
///
/// Why: the ticket fixes a five-row floor so the operator always sees a useful
/// slice of the fleet even on a short terminal; the render path sizes the list
/// region with this as the lower bound and ratatui scrolls within it. Naming the
/// floor here single-sources it between the layout constraint and its test.
/// What: the literal `5`.
/// Test: `min_visible_rows_is_five`.
pub const MIN_VISIBLE_ROWS: usize = 5;

/// How many rows a single Page-Up / Page-Down jump moves the selection.
///
/// Why: Page scrolling should advance by roughly a viewport so a long fleet is
/// traversable in a few keystrokes; tying the jump to [`MIN_VISIBLE_ROWS`] keeps
/// it proportional to the guaranteed-visible slice without needing the live
/// (terminal-dependent) viewport height in this pure model.
/// What: equals [`MIN_VISIBLE_ROWS`].
/// Test: `page_down_jumps_by_page_size`, `page_up_jumps_by_page_size`.
pub const PAGE_JUMP: usize = MIN_VISIBLE_ROWS;

/// Selection + scroll-offset model for the unified coordinator list.
///
/// Why: the unified list is `[controller row 0] + [session rows…]`; the operator
/// navigates it with the arrow keys and Page-Up/Page-Down, and the selection +
/// scroll offset must survive the timer re-polls (a refresh re-derives the row
/// count, not the cursor). ratatui's [`ListState`] is the canonical home for both
/// the selected index and the scroll offset and auto-scrolls to keep the
/// selection on screen, so [`ListNav`] owns one and exposes only pure, bounded
/// movers over it — no terminal, no IO — making every navigation rule unit
/// testable.
/// What: holds the [`ListState`] (selected + offset) and the current `len`
/// (total selectable rows, always ≥ 1 because the controller row always exists).
/// Mutators saturate at `0..len`; [`Self::sync_len`] re-clamps after a poll.
/// Test: `super::tests` — `nav_*` cases cover movement saturation, page jumps,
/// and preservation/clamp across `sync_len`.
#[derive(Debug, Clone)]
pub struct ListNav {
    /// ratatui list state: the selected row index AND the scroll offset.
    state: ListState,
    /// Total selectable rows (controller + sessions); always ≥ 1.
    len: usize,
}

impl Default for ListNav {
    /// Why: the list always has at least the controller row, and row 0 is the
    /// natural initial selection (mirrors the old `selected: 0` default).
    /// What: a [`ListNav`] with `len = 1` and row 0 selected.
    /// Test: `nav_default_selects_controller_row`.
    fn default() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self { state, len: 1 }
    }
}

impl ListNav {
    /// The currently selected row index (`0` = controller).
    ///
    /// Why: callers (the renderer's two-column branch, focus actions) need the
    /// selected index without reaching into the wrapped [`ListState`].
    /// What: returns the `ListState` selection, defaulting to `0` (the controller
    /// row always exists, so `None` is treated as row 0).
    /// Test: `nav_default_selects_controller_row`, `nav_up_down_saturate`.
    pub fn selected(&self) -> usize {
        self.state.selected().unwrap_or(0)
    }

    /// The current scroll offset (index of the first visible row).
    ///
    /// Why: tests assert the offset is PRESERVED across `sync_len`; exposing it
    /// gives them a pure seam without rendering a frame.
    /// What: returns the wrapped `ListState` offset.
    /// Test: `nav_sync_len_preserves_selection_and_offset`.
    pub fn offset(&self) -> usize {
        self.state.offset()
    }

    /// Mutable access to the wrapped [`ListState`] for `render_stateful_widget`.
    ///
    /// Why: the render path hands the same `ListState` to
    /// `frame.render_stateful_widget`, which both reads the selection/offset and
    /// updates the offset to keep the selection visible within the live viewport.
    /// Borrowing it mutably keeps that ratatui-owned auto-scroll behaviour intact.
    /// What: returns `&mut ListState`.
    /// Test: side-effect-only render seam; exercised by launching the TUI.
    pub fn state_mut(&mut self) -> &mut ListState {
        &mut self.state
    }

    /// Select a specific row, clamped into `0..len`.
    ///
    /// Why: focusing a numbered session (STUI-1) or restoring a saved cursor
    /// needs a direct, bounded setter; ratatui auto-scrolls the offset on the next
    /// render to bring the chosen row into view.
    /// What: selects `min(row, len - 1)`.
    /// Test: `nav_select_clamps_into_bounds`.
    pub fn select(&mut self, row: usize) {
        let max = self.len - 1;
        self.state.select(Some(row.min(max)));
    }

    /// Re-point the model at a new row count after a poll, preserving the cursor.
    ///
    /// Why: the timer re-poll (and, later, a post-mutation re-poll) rebuilds the
    /// session list, changing the row count; the selection and scroll offset must
    /// survive when still valid and only clamp inward when the list SHRANK past
    /// them. Without this the operator's cursor would jump to the top on every
    /// refresh.
    /// What: sets `len` to `max(1, row_count)` (the controller row always
    /// exists), then clamps the selection to `0..len` and the offset to
    /// `0..len` so neither indexes past the new end. A grown list leaves both
    /// untouched.
    /// Test: `nav_sync_len_preserves_selection_and_offset`,
    /// `nav_sync_len_clamps_on_shrink`.
    pub fn sync_len(&mut self, row_count: usize) {
        self.len = row_count.max(1);
        let max = self.len - 1;
        let sel = self.selected().min(max);
        self.state.select(Some(sel));
        // Clamp the offset too: a shrunk list must not leave the first visible
        // row past the new end. ratatui will still nudge the offset on the next
        // render to keep `sel` visible, but clamping here keeps the pure model
        // self-consistent and unit-testable.
        if self.state.offset() > max {
            *self.state.offset_mut() = max;
        }
    }

    /// Move the selection up one row (saturating at the controller, row 0).
    ///
    /// Why: ↑ / `k` move the highlight toward the controller.
    /// What: selects `selected().saturating_sub(1)`.
    /// Test: `nav_up_down_saturate`.
    pub fn up(&mut self) {
        let next = self.selected().saturating_sub(1);
        self.state.select(Some(next));
    }

    /// Move the selection down one row (saturating at the last row).
    ///
    /// Why: ↓ / `j` move the highlight toward the newest session.
    /// What: selects `min(selected() + 1, len - 1)`.
    /// Test: `nav_up_down_saturate`.
    pub fn down(&mut self) {
        let max = self.len - 1;
        let next = (self.selected() + 1).min(max);
        self.state.select(Some(next));
    }

    /// Jump the selection up by one page (saturating at row 0).
    ///
    /// Why: Page-Up traverses a long fleet quickly without holding ↑.
    /// What: selects `selected().saturating_sub(`[`PAGE_JUMP`]`)`.
    /// Test: `page_up_jumps_by_page_size`, `page_up_saturates_at_top`.
    pub fn page_up(&mut self) {
        let next = self.selected().saturating_sub(PAGE_JUMP);
        self.state.select(Some(next));
    }

    /// Jump the selection down by one page (saturating at the last row).
    ///
    /// Why: Page-Down is the symmetric fast-scroll toward the newest session.
    /// What: selects `min(selected() + `[`PAGE_JUMP`]`, len - 1)`.
    /// Test: `page_down_jumps_by_page_size`, `page_down_saturates_at_bottom`.
    pub fn page_down(&mut self) {
        let max = self.len - 1;
        let next = self.selected().saturating_add(PAGE_JUMP).min(max);
        self.state.select(Some(next));
    }
}
