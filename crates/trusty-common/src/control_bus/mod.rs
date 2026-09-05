//! Shared event types for the control bus — types only, no transport.
//!
//! Why: Owner ruling 2026-09-05 (superseding DOC-73 §4.1 Option C) makes
//!      trusty-console the one and only event bus. Every harness —
//!      trusty-agents, trusty-mpm, trusty-code — is a producer that pushes to
//!      it. Producers and that consumer must agree on the envelope, so the
//!      envelope cannot live inside any one producer: before this module,
//!      reaching these types meant depending on `trusty-agents-common`, which
//!      is a sibling producer, not a shared library. Hoisting the types here
//!      gives all four crates one definition and no producer-to-producer edge.
//! What: Re-exports `HarnessSource` and `LifecycleEvent` (the taxonomy),
//!       `HarnessPayload` and `HarnessEvent` (the envelope), and `Filter` (the
//!       subscriber-side predicate). Nothing here transports an event: no
//!       channel, no process-global sender, no sequence counter. A producer
//!       stamps `seq` and `at` itself and hands the envelope to whatever moves
//!       it; the bus that owns delivery is trusty-console's.
//! Test: `tests` below covers serde round-trips and the filter matrix, and
//!       `tests::control_bus_declares_no_transport` reads this module's own
//!       sources to keep the types-only boundary from eroding.

// #6846: hoisted out of `trusty_agents_common::events`, which keeps its
// in-process channel and stderr relay until slice 9 (#6854) removes them.

mod envelope;
mod filter;
mod lifecycle;

pub use envelope::{HarnessEvent, HarnessPayload};
pub use filter::Filter;
pub use lifecycle::{HarnessSource, LifecycleEvent};

#[cfg(test)]
mod tests;
