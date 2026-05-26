//! Daemon client re-exports for the TUI.
//!
//! Why: the TUI used to carry its own `DaemonClient` and wire-row types. They
//! now live in the shared `trusty-mpm-client` crate so the TUI, the Telegram
//! bot, and the CLI share one transport. This module is the thin compatibility
//! seam: it re-exports the shared types under the paths the dashboard already
//! imports (`client::DaemonClient`, `client::SessionRow`, ...), so the refactor
//! did not have to touch every `use` site.
//! What: `pub use` of the shared client's [`DaemonClient`] plus the wire-row
//! types the dashboard panels deserialize and render.
//! Test: the shared client's own suite (`cargo test -p trusty-mpm-client`)
//! covers construction and wire-shape deserialization.

pub use crate::client::{BreakerRow, DaemonClient, EventRow, LastSeen, SessionRow};
