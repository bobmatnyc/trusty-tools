//! Per-command handlers for `tctl`.
//!
//! Why: Keeping each command in its own submodule prevents `mod.rs` from
//! becoming a monolith (500-line cap; CLAUDE.md) and lets each handler be
//! tested independently.
//!
//! What: Re-exports the public `run_*` functions so `main.rs` can dispatch
//! through a single `crate::commands::*` import. Phase-0 stubs return
//! `NotYetImplemented`; fully-implemented commands (currently only `version`)
//! perform real work.
//!
//! Test: Each module has its own test section; `cargo test -p trusty-controller`
//! runs them all.

pub mod config;
pub mod doctor;
pub mod ensure;
pub mod install;
pub mod lifecycle;
pub mod passthrough;
pub mod port;
pub mod stack;
pub mod status;
pub mod ui;
pub mod updates;
pub mod upgrade;
pub mod version;
