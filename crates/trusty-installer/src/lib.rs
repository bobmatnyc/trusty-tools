//! trusty-installer library crate.
//!
//! Why: Separating all logic into a library crate keeps the binary
//! (`trusty-installer` / `tctl` alias) as a thin dispatcher and makes every
//! command handler unit-testable without spawning a subprocess.
//!
//! What: Re-exports the public submodules: `cli` (clap structs), `commands`
//! (per-command handlers), `download` (prebuilt-binary download layer, Phase 2
//! #1760), `output` (human and JSON renderers), and `scope`
//! (project-scope detection helpers).
//!
//! Test: `cargo test -p trusty-installer` exercises the arg-parsing round-trips
//! in `cli` and the `not_yet_implemented` stub contracts in `commands`.

pub mod cli;
pub mod commands;
pub mod download;
pub mod output;
pub mod scope;
