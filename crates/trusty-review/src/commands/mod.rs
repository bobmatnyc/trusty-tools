//! CLI subcommand handlers extracted from `main.rs`.
//!
//! Why: `main.rs` was approaching and exceeding the 500-line file cap (#610).
//! Extracting the per-subcommand handler functions (run, compare, serve) into
//! this module keeps `main.rs` lean (arg definitions + dispatch only) and gives
//! each handler a focused home.
//!
//! What: re-exports `cmd_run`, `cmd_compare`, and `cmd_serve` (the latter gated
//! behind `http-server`).  Also re-exports the shared helpers `build_deps_async`,
//! `resolve_diff_source_run`, `resolve_diff_source_compare`, and
//! `print_compare_table`/`truncate_str` used by the compare printer.
//!
//! Test: handlers are tested transitively via `runner::tests` (unit) and the
//! CLI smoke-tests in this file's sibling modules.

pub mod compare;
pub mod run;
#[cfg(feature = "http-server")]
pub mod serve;
