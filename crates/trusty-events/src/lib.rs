//! `trusty-events`: the event bus every trusty-* daemon pushes to, run as its
//! own daemon (ADR-0065, epic #9074, #9075).
//!
//! Why: owner ruling 27 made events a first-class system, and ADR-0065 runs
//! the bus in its own launchd-supervised process so a console deploy or a
//! trusty-mpm restart does not drop its subscribers or its ring. The bus core
//! moved here from `trusty-console/src/event_bus/`.
//!
//! What: one Unix socket, `trusty_common::control_bus::events_socket_path()`,
//! carries both directions. A producer's `PushClient` writes newline-delimited
//! `HarnessEvent` frames; a reader writes one `events.subscribe` JSON-RPC
//! request and reads a stream of `SubscribeItem`s. The first line of a
//! connection decides which it is. The bus assigns one `seq` to every accepted
//! event, keeps a bounded ring for live readers, and appends each event to a
//! day-rotated NDJSON log under `<data dir>/trusty-events/event_log/` so `seq`
//! and replay survive a restart.
//!
//! The Rust API is internal to trusty-*, carries no stability promise, and no
//! other workspace crate links it (ADR-0065 decision 1). The wire contract
//! lives in `trusty_common::control_bus`; this crate implements it. The only
//! public surface is [`daemon`], which the `trusty-events` binary runs.
//!
//! Test: `tests/daemon_wire.rs`, `tests/daemon_exit.rs`,
//! `tests/no_other_crate_links_bus.rs`, and the unit tests in `tests.rs`,
//! `serve_tests.rs` and `log/tests.rs`.

// #9075: moved from trusty-console's `event_bus` module.
mod bus;
pub mod daemon;
mod ingest;
mod log;
mod serve;

#[cfg(test)]
mod serve_tests;
#[cfg(test)]
mod tests;

pub use daemon::{DaemonConfig, DaemonError, run};
pub use log::LogError;
pub use serve::ServeError;
