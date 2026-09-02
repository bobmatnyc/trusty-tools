//! CLI subcommand handlers for `trusty-analyzer`.

pub mod daemon;
pub mod daemon_guard;
// #6669: the `report` verb, over trusty-review's embedded report pipeline.
#[cfg(feature = "review")]
pub mod report;
pub mod run;
pub mod service;
pub mod setup;
pub mod socket;
pub mod version;
