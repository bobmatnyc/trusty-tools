//! Service-specific terminal UI for the trusty-memory daemon.
//!
//! Why: operators of the trusty-memory daemon want a focused terminal surface
//! — a palace list, a streaming activity log of dream / drawer / recall
//! events, and a recall query bar — rather than the generic two-daemon
//! dashboard. Living in `trusty-common` behind the `monitor-tui` feature keeps
//! the pure state / rendering testable (issue #34).
//! What: a ratatui app with a 3-zone layout (title bar, PALACES + right-hand
//! split, RECALL input bar). The PALACES list always leads with an "All
//! palaces" entry that fans recalls out across every palace; the right side is
//! split vertically into an ACTIVITY feed (top) and a STATISTICS panel
//! (bottom), both scoped to the selected palace — or aggregated when "All" is
//! selected. It polls the daemon every 2 seconds, subscribes to the `/sse`
//! event stream on startup, runs cross-palace recalls from the input bar on
//! `[Enter]`, and triggers a dream cycle on `[d]`.
//! Test: `cargo test -p trusty-common --features monitor-tui` covers the pure
//! state, the "All" selector, palace row formatting, the activity / statistics
//! line builders, and dream-event logging; `trusty-memory monitor tui`
//! launches the live UI.

pub mod activity;
pub mod event_loop;
pub mod render;
pub mod state;
pub mod view;

/// Type alias that names the shared three-way sort key in the memory-palace
/// domain.
///
/// Why: callers in `trusty-memory` refer to the sort key as `PalaceSortKey`,
/// not the generic `ThreeWaySortKey`, which keeps the API self-documenting.
/// What: re-exports [`crate::monitor::tui_common::ThreeWaySortKey`] under a
/// domain-specific alias.
/// Test: `test_palace_sort_key_cycle` exercises every variant and label.
pub type PalaceSortKey = crate::monitor::tui_common::ThreeWaySortKey;

// Re-export the public API so external callers remain unchanged.
pub use activity::{
    DREAMING_SPINNER, INDEXING_SPINNER, PalaceActivity, activity_label, format_relative_time,
    palace_activity_state, spinner_tick,
};
pub use event_loop::apply_memory_event;
pub use render::{render, run_with_url};
pub use state::{
    DRAWER_PAGE_SIZE, DREAM_BACKOFF_INITIAL, DREAM_BACKOFF_MAX, DrawerListState, DreamBackoff,
    MemoryFocus, MemoryTuiState, RECALL_TOP_K,
};
pub use view::{
    ALL_LABEL, KEY_HINT, PalaceListRow, VERSION, drawer_detail_body, drawer_panel_lines,
    filtered_sorted_palaces, format_drawer_row, help_text, navigate_down_visible,
    navigate_up_visible, palace_has_content, palace_lines, palace_lines_at, palace_row,
    palace_row_with_activity, sort_label, stats_lines, title_line, visible_palace_ids,
    visible_selected_row,
};

/// Run the trusty-memory monitor TUI.
///
/// Why: the single entry point the `monitor tui` subcommand of `trusty-memory`
/// calls.
/// What: resolves the daemon URL from the service lock file and delegates to
/// [`run_with_url`].
/// Test: the pure pieces are unit-tested; this thin glue is exercised by
/// launching the UI.
pub async fn run() -> anyhow::Result<()> {
    use crate::monitor::memory_client::resolve_memory_url;
    run_with_url(resolve_memory_url()).await
}

#[cfg(test)]
mod tests;
