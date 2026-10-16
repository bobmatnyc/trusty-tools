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
// #8257: the record view the dispatch deny and `--list` share, and the OS
// probe the repair asks before it writes.
// #6556: stale a quiet Running record the OS shows is gone.
pub(crate) mod delegation_quiet_stale;
pub mod delegation_records;
pub mod delegation_repair;
// #8531: the repair caller, established from the socket's kernel peer pid.
pub mod delegation_repair_caller;
pub(crate) mod delegation_repair_probe;
pub mod delegation_tracker;
pub mod hook_service;
// #8994 S3a: the ledger reclaim pass, run inside the merged-PR reclaim tick.
pub(crate) mod ledger_reclaim;
// #7504: the ONE place the merged-PR reclaim's production probes are assembled,
// plus the daemon loop that runs it automatically after a merge.
pub mod merged_pr_reclaim;
pub mod pairing_service;
pub mod session_service;
// #8451: the disk-budget sweep over the builder slot pool.
pub(crate) mod slot_pool_evict;
// #6556: the reader for the `SubagentStop` records a hook parked on disk when
// this daemon was unreachable.
pub mod stop_spool_drain;
// #8059: the durable, cross-process half of that status — `tm doctor` runs
// daemonless, so the in-memory timings alone reported every sweep idle.
pub(crate) mod sweep_state_file;
// #7965: the single background-maintenance lane both sweeps take, and the
// last-pass timings `tm doctor` reports.
pub(crate) mod sweep_status;
pub mod tmux_service;
// #8994 S2b: records `git worktree add|remove|prune` run in a Bash tool call.
pub mod worktree_command_ingest;
// #8994 S2a: records agent-isolation trees in the worktree ledger from hooks.
pub mod worktree_ledger_ingest;
// #8994 S2c: `removed` from the three removal routes, on evidence only.
pub(crate) mod worktree_removal_ledger;

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
