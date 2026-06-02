//! Subcommand implementations for the `tga` binary.
//!
//! Each module exposes a single `run` function invoked by `main.rs` after
//! the CLI is parsed and the database is opened.

pub mod aliases;
pub mod analyze;
pub mod args;
pub mod author;
pub mod backfill;
pub mod classify;
pub mod collect;
pub mod date_range;
pub mod deployments;
pub mod dora;
pub mod incidents;
pub mod install;
pub mod override_cmd;
pub mod pr_metrics;
pub mod report;
pub mod rules;
