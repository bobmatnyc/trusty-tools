//! Managed session subsystem.
//!
//! Why: the session manager tracks every agent session the MPM daemon has
//! spawned so that operators can inspect, control, and recover sessions after
//! a daemon restart without losing track of running work.
//! What: re-exports the public types and entry points from `record`, `store`,
//! and `manager` so callers import from one stable path.
//! Test: each sub-module carries its own unit tests; manager integration is
//! tested in `manager::tests`.

pub mod adopt;
pub mod create;
pub mod decommission;
pub mod dedup;
pub mod delete;
pub mod driver;
pub mod hook_sync;
pub mod manager;
pub mod naming;
pub mod prune;
pub mod reactivate;
mod reconcile;
pub mod record;
pub mod restart_ops;
mod resume_workdir;
pub mod search_gc;
pub mod session_guard;
pub mod snapshot;
pub mod store;
pub mod task_inject;
pub mod workspace_guard;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod restart_tests;

#[cfg(test)]
mod reactivate_tests;

#[cfg(test)]
mod backfill_tests;

#[cfg(test)]
mod decommission_worktree_tests;

#[cfg(test)]
mod delete_tests;

#[cfg(test)]
mod liveness_tests;

#[cfg(test)]
mod reap_orphaned_worktrees_tests;

#[cfg(test)]
mod naming_tests;

#[cfg(test)]
mod resume_reattach_tests;

#[cfg(test)]
mod set_source_id_tests;

#[cfg(test)]
mod set_deliverable_id_tests;

#[cfg(test)]
mod dedup_tests;

#[cfg(test)]
mod runtime_exit_reconcile_tests;

#[cfg(test)]
mod reload_error_tests;

#[cfg(test)]
mod pane_scoped_tests;

#[cfg(test)]
mod adopt_existing_tests;

/// Real tmux driver adapter — only available when the `daemon` feature (and thus
/// the daemon's `TmuxDriver`) is compiled in.
#[cfg(feature = "daemon")]
pub mod real_tmux;

pub use manager::{ManagedError, ManagedTmuxDriver, ReconcileReport, SessionManager};
pub use prune::{MAX_EPHEMERAL_AGE_HOURS, PruneAction, PruneFilter, PruneOutcome, PrunedSession};
pub use record::{ManagedSessionId, ManagedSessionState, RecordError, SessionRecord};
pub use session_guard::TmuxSessionGuard;
pub use store::{SessionStore, StoreError};
pub use task_inject::should_inject_task;

#[cfg(feature = "daemon")]
pub use real_tmux::RealTmuxDriver;

// FakeNoopTmuxDriver is compiled unconditionally under `daemon` (not #[cfg(test)])
// so that integration tests in `tests/` can reference it via
// `DaemonState::with_root_isolated_managed`. It is never called in production.
#[cfg(feature = "daemon")]
pub use real_tmux::FakeNoopTmuxDriver;
