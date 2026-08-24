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
// #4743: the single capability every destructive index DELETE must hold.
mod index_delete_guard;
pub mod injection_status;
pub mod manager;
pub mod naming;
mod numbering;
pub mod prune;
pub mod reactivate;
mod reconcile;
pub mod record;
pub mod rename;
pub mod restart_ops;
pub(crate) mod resume_workdir;
/// Age-based eviction of terminal records and the slot numbers they hold.
pub mod retention;
pub mod search_gc;
pub mod session_guard;
pub mod slots;
pub mod snapshot;
pub mod store;
// #5007: the read-failure fallback's severity split and the `store_health`
// accessor that makes it visible instead of silent.
pub mod store_health;
// #5007: corruption detection and the offline truncate-to-last-valid-document
// repair behind `tm repair session-store`.
pub mod store_integrity;
pub mod task_inject;
pub mod workspace_guard;
// #4311: the OS-level "is a process standing in here?" gate — the one removal
// check that does not read a registry trusty-mpm or git wrote.
pub(crate) mod worktree_liveness;
mod worktree_nested;
pub(crate) mod worktree_ownership;
// #2919: merged-PR reclamation + the disk accounting `tm doctor` reports.
pub(crate) mod worktree_reclaim;
// #4732: the tri-state "does git still hold state here?" classifier that gates
// every raw directory removal on the worktree teardown path.
mod worktree_protection;
// #2919: the survey and the fresh-recheck delete loop that acts on it.
pub(crate) mod worktree_reclaim_sweep;
pub(crate) mod worktree_reconcile;
pub(crate) mod worktree_registry;
pub mod worktree_safety;

#[cfg(test)]
mod tests;

// #4743: the destructive index-DELETE guard, asserted against a live,
// accepting loopback daemon rather than a dead port.
#[cfg(test)]
mod search_gc_guard_tests;

#[cfg(test)]
mod restart_tests;

#[cfg(test)]
mod reactivate_tests;

#[cfg(test)]
mod backfill_tests;

#[cfg(test)]
mod decommission_tests;

#[cfg(test)]
mod decommission_worktree_tests;

#[cfg(test)]
mod delete_tests;

#[cfg(test)]
mod rename_tests;

#[cfg(test)]
mod liveness_tests;

#[cfg(test)]
mod reap_orphaned_worktrees_tests;

#[cfg(test)]
mod naming_tests;

#[cfg(test)]
mod resume_reattach_tests;

#[cfg(test)]
mod server_up_tests;

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

#[cfg(test)]
mod injection_status_tests;

#[cfg(test)]
mod slots_tests;

#[cfg(test)]
mod send_input_gate_tests;

// #6194: a stop somebody asked for must survive both automatic resume paths.
#[cfg(test)]
mod stop_cause_tests;

/// Shared real-git fixtures for the #4091 dirty-worktree-guard tests, used by
/// both `worktree_safety::worktree_safety_tests` and `prune::orphan_tests`.
#[cfg(test)]
pub(crate) mod worktree_git_fixture;

/// Real tmux driver adapter — only available when the `daemon` feature (and thus
/// the daemon's `TmuxDriver`) is compiled in.
#[cfg(feature = "daemon")]
pub mod real_tmux;

pub use injection_status::InjectionStatus;
pub use manager::{ManagedError, ManagedTmuxDriver, ReconcileReport, SessionManager};
pub use prune::{MAX_EPHEMERAL_AGE_HOURS, PruneAction, PruneFilter, PruneOutcome, PrunedSession};
pub use record::{ManagedSessionId, ManagedSessionState, RecordError, SessionRecord, StopCause};
pub use retention::{
    RetentionDebounce, RetentionOutcome, RetentionVerdict, TERMINAL_RECORD_RETENTION_DAYS,
    retention_verdict,
};
pub use session_guard::TmuxSessionGuard;
pub use slots::{NumberedSlot, SlotRegistry};
pub use store::{SessionStore, StoreDegradation, StoreError};
pub use task_inject::should_inject_task;
pub use worktree_safety::{DirtyWorktree, DirtyWorktreePolicy};

#[cfg(feature = "daemon")]
pub use real_tmux::RealTmuxDriver;

// FakeNoopTmuxDriver is compiled unconditionally under `daemon` (not #[cfg(test)])
// so that integration tests in `tests/` can reference it via
// `DaemonState::with_root_isolated_managed`. It is never called in production.
#[cfg(feature = "daemon")]
pub use real_tmux::FakeNoopTmuxDriver;
