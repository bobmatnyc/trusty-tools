//! The console-hosted event bus core: UDS ingest, a bounded ring, and a
//! subscriber fan-out (issue #6848, DOC-73 §4.1-§4.3).
//!
//! Why: owner ruling 2026-09-05 makes `trusty-console` the one and only event
//! bus in the workspace — every harness (`trusty-mpm`, `trusty-code`,
//! `trusty-agents`, `trusty-analyze`) pushes `HarnessEvent` frames here instead
//! of hosting its own broadcast channel (DOC-73 §4.1). This module is that
//! bus's core: the socket producers dial, the bounded in-memory ring later
//! slices read, and the counters that make ingest, dedup and eviction
//! observable. It does not yet cover the two other §4.3 pieces the wider issue
//! describes — the durable day-rotated NDJSON log and the console-assigned
//! `seq` — which land in a follow-up PR; a partial PR against #6848 is
//! deliberate here, not an oversight.
//!
//! What: [`bus::EventBus`] holds the ring (capacity configurable, default
//! 8192 per DOC-73 §4.3), dedups by [`trusty_common::control_bus::EventId`],
//! and exposes a [`tokio::sync::broadcast`] subscriber API for the SSE fan-out
//! a later slice (#6851) builds on. [`ingest::bind_ingest`] and
//! [`ingest::serve_ingest`] are the UDS listener: newline-delimited
//! `HarnessEvent` JSON, one connection per producer, a malformed line dropped
//! and logged rather than closing the connection.
//!
//! Test: `tests.rs` — ingest over a real tempdir socket, dedup, eviction at
//! capacity, and a malformed line surviving to ingest the next valid one.

pub(crate) mod bus;
pub(crate) mod ingest;

#[cfg(test)]
mod tests;

pub(crate) use bus::{EventBus, EventBusConfig};
pub(crate) use ingest::{bind_ingest, ingest_socket_path, serve_ingest};
