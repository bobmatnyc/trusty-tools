//! CLI subcommand handlers.
//!
//! Why: `main()` was a 2.7k-line file mixing clap argument definitions with
//! subcommand implementations and ~50 helper functions. This module splits
//! each `Commands::*` variant into its own handler plus a set of shared
//! support modules (`daemon_utils`, `format`, `index_resolve`,
//! `reindex_engine`, `doctor_checks`, `doctor_pipeline`) so `main.rs` becomes
//! a thin clap-to-handler dispatcher.
//!
//! What: one module per subcommand, plus a set of shared helper modules.
//! Handlers take the parsed argument fields plus any global flags they need
//! (`index`, `json`). They return `Result<()>` and bubble user-facing errors
//! via `anyhow::bail!` / `Err(...)` — the central `main()` dispatcher prints
//! the friendly red-✗ line and chooses the exit code (issue #104, so
//! handlers are testable without forking a process).
//!
//! Test: `cargo build && cargo test --workspace` — no behaviour change; the
//! refactor is purely structural.

// Shared support modules
pub mod daemon_utils;
pub mod doctor_checks;
pub(crate) mod doctor_pipeline;
pub mod format;
pub mod index_resolve;
pub mod log_rotation;
pub mod reindex_engine;
pub(crate) mod reindex_ui;

// Shared startup sanity checks (Fix D, issue #747)
pub mod startup_checks;

// Per-subcommand handlers
pub mod add;
pub mod cleanup;
pub mod config;
pub mod convert;
pub mod daemon_guard;
pub mod dashboard;
pub mod discover;
pub mod doctor;
pub mod index;
pub mod index_allowlist;
pub mod index_cwd_resolve;
pub(crate) mod index_persist;
pub mod index_remove;
pub mod index_status;
pub mod init;
pub mod integrate;
pub mod list;
pub mod migrate;
pub mod migrate_redb;
pub mod migrate_storage;
pub mod monitor;
pub mod port;
pub mod prune_orphans;
pub mod query;
pub mod reindex;
pub mod remove;
pub mod search;
pub mod serve;
pub mod service;
pub mod setup;
pub mod start;
pub mod start_restore;
pub mod status;
pub mod stop;
pub mod upgrade;
pub mod watch;
