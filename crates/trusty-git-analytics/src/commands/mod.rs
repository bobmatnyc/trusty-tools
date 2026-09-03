//! Subcommand implementations for the `tga` binary.
//!
//! Each module exposes a single `run` function invoked by `main.rs` after
//! the CLI is parsed and the database is opened.

pub mod aliases;
pub mod analyze;
pub mod args;
pub mod audit;
pub mod author;
pub mod backfill;
pub mod classify;
pub mod collect;
pub mod date_range;
pub mod deployments;
pub mod dora;
pub mod incidents;
// #5218: read-only database inspection and the data-handling attestation.
pub mod inspect;
pub mod install;
// #5216: the non-interactive install path and the config emission both front
// ends share, split out to keep `install.rs` under the SLOC cap.
pub(crate) mod install_flags;
pub mod install_plan;
pub mod jira;
pub mod override_cmd;
pub mod pr_metrics;
pub mod profile;
pub mod report;
pub mod rules;
pub mod tui;
