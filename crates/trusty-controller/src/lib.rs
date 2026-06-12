//! trusty-controller library crate.
//!
//! Why: Separating all logic into a library crate keeps the binary (`tctl`) as
//! a thin dispatcher and makes every command handler unit-testable without
//! spawning a subprocess.
//!
//! What: Re-exports the public submodules: `cli` (clap structs), `commands`
//! (per-command handlers), `output` (human and JSON renderers), and `scope`
//! (project-scope detection helpers).
//!
//! Test: `cargo test -p trusty-controller` exercises the arg-parsing round-trips
//! in `cli` and the `not_yet_implemented` stub contracts in `commands`.

pub mod cli;
pub mod commands;
pub mod output;
pub mod scope;
