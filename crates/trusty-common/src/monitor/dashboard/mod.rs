//! Unified monitor dashboard state and rendering.
//!
//! Why: the monitor TUI watches two independent daemons (trusty-search and
//! trusty-memory) side by side; keeping the pure state and layout logic here —
//! separate from the event loop and HTTP polling — makes the layout decisions
//! and number formatting unit-testable without a terminal.
//! What: [`DashboardState`] holds a [`DaemonPanel`] per daemon plus focus and
//! help flags; [`render`] draws the ratatui frame; the free `*_constraints` /
//! `format_*` helpers are the pure pieces the test suite asserts on.
//! Test: `cargo test -p trusty-monitor-tui` covers layout selection, offline
//! rendering, and uptime/count formatting without a terminal.

mod format;
mod render;
#[cfg(test)]
mod tests;
mod types;

pub use format::{format_count, format_uptime, help_text, panel_layout, status_badge, truncate};
pub use render::{memory_panel_lines, render, search_panel_lines};
pub use types::{
    DaemonPanel, DashboardState, Focus, IndexRow, KEY_HINT, MemoryData, PalaceRow, PanelStatus,
    SearchData, VERSION, WIDE_LAYOUT_MIN_COLS,
};
