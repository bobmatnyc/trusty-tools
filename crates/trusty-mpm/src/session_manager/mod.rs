//! Managed session subsystem.
//!
//! Why: the session manager tracks every agent session the MPM daemon has
//! spawned so that operators can inspect, control, and recover sessions after
//! a daemon restart without losing track of running work.
//! What: re-exports the public types and entry points from `record`, `store`,
//! and `manager` so callers import from one stable path.
//! Test: each sub-module carries its own unit tests; manager integration is
//! tested in `manager::tests`.

pub mod manager;
pub mod record;
pub mod store;

#[cfg(test)]
mod tests;

/// Real tmux driver adapter — only available when the `daemon` feature (and thus
/// the daemon's `TmuxDriver`) is compiled in.
#[cfg(feature = "daemon")]
pub mod real_tmux;

pub use manager::{ManagedError, ManagedTmuxDriver, ReconcileReport, SessionManager};
pub use record::{ManagedSessionId, ManagedSessionState, RecordError, SessionRecord};
pub use store::{SessionStore, StoreError};

#[cfg(feature = "daemon")]
pub use real_tmux::RealTmuxDriver;
