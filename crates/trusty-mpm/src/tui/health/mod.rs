//! Combined search + memory health screen (`[2]`) for the trusty-mpm TUI.
//!
//! Why: operators want one glance to confirm that the two daemons the
//! coordinator depends on — trusty-search and trusty-memory — are alive and
//! healthy, without leaving the TUI to run two `status` commands. Keeping the
//! poller, the typed wire shapes, and the pure rendering helpers here (away
//! from the coordinator chat in `dashboard.rs`) keeps both surfaces small and
//! independently testable.
//! What: [`HealthClient`] is a typed `reqwest` transport for one daemon's
//! `/health` + list endpoints; [`PanelData`] is the projected per-daemon
//! payload; [`PanelState`] is `Connecting` / `Online` / `Offline`;
//! [`HealthScreen`] holds both panels plus focus and renders the side-by-side
//! layout. A background tokio task drives polling and pushes [`HealthUpdate`]s
//! down a channel into the TUI event loop.
//!
//! This module is split into focused submodules (issue #607): `types` holds the
//! pure data types and constants, `activity` the palace activity/spinner
//! helpers, `format` the string formatters and panel-body builders, `probes`
//! the HTTP transport and JSON projections, `screen` the [`HealthScreen`] state
//! methods and pure line builders, and `render` the ratatui widget assembly.
//! This `mod.rs` is a thin facade that re-exports the public surface (so the
//! external `health::*` path is unchanged) and delegates the test suite to
//! the sibling `tests.rs` file.
//!
//! Test: `cargo test -p trusty-mpm` covers the wire projections, panel
//! line building, the focus toggle, and a `TestBackend` render smoke test.

pub mod activity;
pub mod format;
pub mod probes;
pub mod render;
pub mod screen;
pub mod types;

pub use activity::{activity_color, palace_activity, spinner_frame};
pub use format::{
    ascii_bar, format_bytes, format_count, format_relative_time, format_rss, format_uptime,
    format_with_commas, memory_panel_lines, search_panel_lines,
};
pub use probes::client_for;
pub use render::render;
pub use screen::{
    collections_lines, collections_lines_at_tick, header_lines, health_tab_lines, index_tab_lines,
    palace_index_tab_lines, service_name, tab_bar,
};
pub use types::{
    CollectionRow, DEFAULT_MEMORY_URL, DEFAULT_SEARCH_URL, Daemon, HealthClient, HealthScreen,
    HealthTab, HealthUpdate, LOG_BUFFER_CAP, LogBuffer, POLL_INTERVAL, PalaceActivity, PanelData,
    PanelState,
};

#[cfg(test)]
mod tests;
