//! Command handler modules for the `tm` binary.
//!
//! Why: splitting each subcommand group into its own file keeps every handler
//! file well under the 500-line cap and makes the handler surface easy to
//! navigate.
//! What: re-exports handler modules — `daemon`, `install`, `launch`,
//! `misc`, `project`, `services`, `session`, `telegram`.
//! Test: each module has its own unit tests; integration coverage lives in
//! `tests.rs`.

pub(crate) mod daemon;
pub(crate) mod install;
pub(crate) mod launch;
pub(crate) mod misc;
pub(crate) mod project;
pub(crate) mod services;
pub(crate) mod session;
pub(crate) mod telegram;
