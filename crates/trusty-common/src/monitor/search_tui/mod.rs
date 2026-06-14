//! Service-specific terminal UI for the trusty-search daemon.
//!
//! Why: operators of the trusty-search daemon want a focused, live terminal
//! surface — an index list, a streaming activity log of reindex/search events,
//! and a query bar — rather than the generic two-daemon dashboard. Living in
//! `trusty-common` behind the `monitor-tui` feature keeps the pure state /
//! rendering testable without a separate published crate (issue #34).
//! What: a ratatui app with a 3-zone layout (title bar, INDEXES + right-hand
//! split, SEARCH input bar). The INDEXES list always leads with an "All
//! indexes" entry that fans queries out across every index; the right side is
//! split vertically into an ACTIVITY feed (top) and a STATISTICS panel
//! (bottom), both scoped to the selected index — or aggregated when "All" is
//! selected. It polls the daemon every 2 seconds, streams reindex progress
//! over SSE on `[r]`, and runs hybrid searches from the input bar on `[Enter]`.
//! Input is polled every 50 ms so keys feel instant.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers the pure
//! state, log capacity, selection clamp, the "All" selector, and the
//! activity / statistics line builders; `trusty-search monitor tui` launches
//! the live UI.

mod event_loop;
mod nav;
mod render;
mod state;
#[cfg(test)]
mod tests;

pub use event_loop::{ScopedReindexEvent, apply_reindex_event, run, run_with_url};
pub use nav::{
    filtered_sorted_indexes, navigate_down_visible, navigate_up_visible, new_log_lines_since,
    visible_index_ids, visible_selected_row,
};
pub use render::{
    ALL_LABEL, IndexListRow, IndexSortKey, KEY_HINT, SearchFocus, VERSION, format_bytes, help_text,
    index_lines, render, sort_label, stats_lines, title_line,
};
pub use state::SearchTuiState;
