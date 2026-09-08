//! The console-hosted event bus core: UDS ingest, a bounded ring, a durable
//! log, and a subscriber fan-out (issue #6848, DOC-73 §4.1-§4.3).
//!
//! Why: owner ruling 2026-09-05 makes `trusty-console` the one and only event
//! bus in the workspace — every harness (`trusty-mpm`, `trusty-code`,
//! `trusty-agents`, `trusty-analyze`) pushes `HarnessEvent` frames here instead
//! of hosting its own broadcast channel (DOC-73 §4.1). This module is that
//! bus's core: the socket producers dial, the bounded in-memory ring later
//! slices read, the day-rotated durable NDJSON log slice 3b (this PR) adds,
//! and the counters that make ingest, dedup and eviction observable.
//!
//! What: [`bus::EventBus`] holds the ring (capacity configurable, default
//! 8192 per DOC-73 §4.3), dedups by [`trusty_common::control_bus::EventId`],
//! assigns the console-owned `seq` DOC-73 §4.3 decides, hands each accepted
//! frame to the durable log (`log` submodule), and exposes a
//! [`tokio::sync::broadcast`] subscriber API carrying [`bus::BusFrame`] for
//! the SSE fan-out a later slice (#6851) builds on. [`ingest::bind_ingest`]
//! and [`ingest::serve_ingest`] are the UDS listener: newline-delimited
//! `HarnessEvent` JSON, one connection per producer, a malformed line dropped
//! and logged rather than closing the connection. `log` is slice 3b's own
//! module — see its doc for the durable-log contract in full.
//!
//! Test: `tests.rs` — ingest over a real tempdir socket, dedup, eviction at
//! capacity, seq assignment, and a malformed line surviving to ingest the
//! next valid one. `log/tests.rs` — seq recovery, truncated-tail tolerance,
//! replay-with-gap, persisted-flag semantics, rotation continuity,
//! backpressure.

pub(crate) mod bus;
pub(crate) mod ingest;
pub(crate) mod log;

#[cfg(test)]
mod tests;

pub(crate) use bus::{EventBus, EventBusConfig};
pub(crate) use ingest::{bind_ingest, ingest_socket_path, serve_ingest};
pub(crate) use log::{DurableLog, LogConfig};
