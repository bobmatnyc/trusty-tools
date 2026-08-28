//! CLI subcommand handlers extracted from `main.rs`.
//!
//! Why: `main.rs` was approaching and exceeding the 500-line file cap (#610).
//! Extracting the per-subcommand handler functions into this module keeps
//! `main.rs` lean (arg definitions + dispatch only) and gives each handler a
//! focused home.
//!
//! What: re-exports `cmd_run`, `cmd_compare`, and `cmd_mcp_stdio` (the last
//! gated behind `mcp`). Also re-exports the shared helpers `build_deps_async`,
//! `resolve_diff_source_run`, `resolve_diff_source_compare`, and
//! `print_compare_table`/`truncate_str` used by the compare printer.
//! `diff_source` holds the `--base`/`--head`/`--local-diff` resolution shared
//! by `run` and `compare` (issue #2993). `cmd_calibrate` (calibration harness,
//! #1422) is always present.
//!
//! #6290: `serve` (the UDS review daemon), `socket` (which reported that
//! daemon's socket path) and `service` (its launchd wrapper) are gone. Review
//! runs per invocation — `run` is the entry point that covers what the daemon's
//! `review.run` method served — and `mcp_stdio` is the one long-lived mode
//! left, which is a stdio pipe rather than a listener.
//!
//! Test: handlers are tested transitively via `runner::tests` (unit) and the
//! CLI smoke-tests in this file's sibling modules.

pub mod calibrate;
pub mod compare;
pub(crate) mod diff_source;
#[cfg(feature = "mcp")]
pub mod mcp_stdio;
pub mod run;
