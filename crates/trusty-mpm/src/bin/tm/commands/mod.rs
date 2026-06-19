//! Command handler modules for the `tm` binary.
//!
//! Why: splitting each subcommand group into its own file keeps every handler
//! file well under the 500-line cap and makes the handler surface easy to
//! navigate.
//! What: re-exports handler modules — `daemon`, `install`, `launch`,
//! `managed`, `managed_route`, `meta`, `misc`, `project`, `services`,
//! `session`, `supervisor`, `telegram`.
//! Test: each module has its own unit tests; integration coverage lives in
//! `tests.rs`.

pub(crate) mod daemon;
pub(crate) mod install;
pub(crate) mod issue;
pub(crate) mod launch;
pub(crate) mod managed;
pub(crate) mod managed_route;
pub(crate) mod meta;
pub(crate) mod misc;
pub(crate) mod project;
pub(crate) mod prune;
pub(crate) mod repair;
pub(crate) mod serve_stdio;
pub(crate) mod services;
pub(crate) mod session;
pub(crate) mod sm_serve;
pub(crate) mod supervisor;
pub(crate) mod telegram;
pub(crate) mod ticket;
pub(crate) mod watch;
