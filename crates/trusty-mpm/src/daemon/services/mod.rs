//! Daemon domain services.
//!
//! Why: the HTTP handlers in `api.rs` had grown to embed session lifecycle
//! rules, the hook overseer pipeline, tmux I/O policy, and bot pairing logic
//! directly in the request functions, leaving a 2300-line file where business
//! logic and transport were inseparable. Extracting that logic into focused,
//! HTTP-agnostic services makes each rule unit-testable without spinning up an
//! axum request, and shrinks every handler to deserialize-call-serialize.
//! What: this module groups the four domain services and re-exports their
//! public types so call sites use `crate::daemon::services::SessionService` etc.
//! Test: each submodule carries its own `#[cfg(test)]` suite; run them with
//! `cargo test -p trusty-mpm-daemon services`.

pub mod agent_worktree_reap;
pub mod delegation_repair;
pub mod delegation_tracker;
pub mod hook_service;
// #7504: the ONE place the merged-PR reclaim's production probes are assembled,
// plus the daemon loop that runs it automatically after a merge.
pub mod merged_pr_reclaim;
pub mod pairing_service;
pub mod session_service;
// #6556: the reader for the `SubagentStop` records a hook parked on disk when
// this daemon was unreachable.
pub mod stop_spool_drain;
// #7965: the single background-maintenance lane both sweeps take, and the
// last-pass timings `tm doctor` reports.
pub(crate) mod sweep_status;
pub mod tmux_service;

// #7965: the cross-sweep latency contract — a running sweep never pushes a
// request past the client's discovery budget. It spans both sweeps, so it lives
// beside neither.
#[cfg(test)]
#[path = "sweep_latency_tests.rs"]
mod sweep_latency_tests;

pub use hook_service::{HookDecision, HookService};
pub use pairing_service::{PairCode, PairStatus, PairingService};
pub use session_service::{PauseResult, ReapResult, SessionService};
pub use tmux_service::TmuxService;
