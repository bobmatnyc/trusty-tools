//! Service-specific monitor TUIs for the trusty-search and trusty-memory daemons.
//!
//! Why: operators of each daemon want a focused terminal surface tailored to
//! that service — the trusty-search TUI shows indexes plus a live reindex /
//! search activity log; the trusty-memory TUI shows palaces plus a dream /
//! recall activity log. A single unified two-panel dashboard could not give
//! either service the depth (per-index reindex streaming, per-palace recall)
//! operators need, so issue #34 split it into two dedicated TUIs. Living in
//! `trusty-common` behind the `monitor-tui` feature keeps the event loops,
//! rendering, and HTTP transport in separate, independently testable modules
//! without shipping a separate published crate.
//! What: [`search_tui`] and [`memory_tui`] are the two ratatui apps;
//! [`search_client`] and [`memory_client`] are their typed HTTP transports;
//! [`dashboard`] holds the shared wire-shape structs and number formatters;
//! [`utils`] holds the shared activity log, status enum, and formatters.
//! `trusty-search monitor tui` calls [`search_tui::run`]; `trusty-memory
//! monitor tui` calls [`memory_tui::run`].
//! Test: `cargo test -p trusty-common --features monitor-tui` covers the pure
//! state, rendering, and client pieces of both TUIs.
//!
//! [`search_tui`]: crate::monitor::search_tui
//! [`memory_tui`]: crate::monitor::memory_tui
//! [`search_client`]: crate::monitor::search_client
//! [`memory_client`]: crate::monitor::memory_client
//! [`dashboard`]: crate::monitor::dashboard
//! [`utils`]: crate::monitor::utils
//! [`search_tui::run`]: crate::monitor::search_tui::run
//! [`memory_tui::run`]: crate::monitor::memory_tui::run

pub mod dashboard;
pub mod memory_client;
pub mod memory_tui;
pub mod search_client;
pub mod search_tui;
#[cfg(feature = "monitor-tui")]
pub mod tui_common;
pub mod utils;
