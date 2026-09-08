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
//!       `HarnessPayload` and `HarnessEvent` (the envelope), `EventId` (the
//!       envelope's `id`/`parent_id` type, issue #6847), `Filter` (the
//!       subscriber-side predicate), the `ActionEvent` taxonomy and its
//!       supporting types (`ActionMeta`, `Actor`, `ObjectRef`, `ObjectType`,
//!       `PathRef`, and the five phase enums, issue #6847, DOC-73 §3.2), and
//!       — behind the `uds` feature, unix only — `PushClient` (the buffered
//!       producer-side transport to console's ingest socket, DOC-73 §4).
//!       Everything except `PushClient` is a type with no transport: no
//!       channel, no process-global sender, no sequence counter. A producer
//!       stamps `seq`, `at`, and `id` itself and hands the envelope to
//!       `PushClient`, which buffers and dials out; the bus that assigns
//!       `seq` on arrival, orders, retains, and fans events back out is
//!       trusty-console's alone.
//! Test: `tests` below covers serde round-trips and the filter matrix, and
//!       `tests::control_bus_declares_no_transport` reads this module's own
//!       sources to keep the types-only boundary from eroding — `PushClient`
//!       is the one deliberate, scanned-and-allowed exception (its own
//!       module doc explains why it still satisfies that scan); its own
//!       behavioral tests live beside it in `push_client.rs`.

// #6846: hoisted out of `trusty_agents_common::events`, which keeps its
// in-process channel and stderr relay until slice 9 (#6854) removes them.
// #6847: `event_id` added — `uuid` became a mandatory dependency of this
// crate (previously optional, gated behind `rpc`/`daemon-token`/etc.) because
// `HarnessEvent` is ungated and now always carries an `EventId`. `action`
// (the `HarnessPayload::Action` taxonomy, DOC-73 §3.2) and `push_client` (the
// buffered UDS client, DOC-73 §4) close out the two items #6847 still owed
// after the envelope fields landed in #7150. `push_client` is gated on `uds`
// (and `unix`, matching every other consumer of `crate::uds`) because it is
// the first thing in this module that actually dials a socket; `action` adds
// no dependency, so it stays unconditional like every other type here.

mod action;
mod envelope;
mod event_id;
mod filter;
mod lifecycle;
#[cfg(all(unix, feature = "uds"))]
mod push_client;

pub use action::{
    ActionEvent, ActionMeta, Actor, AgentPhase, CallPhase, FilePhase, ObjectRef, ObjectType,
    PathRef, SessionPhase, WorkflowPhase,
};
pub use envelope::{HarnessEvent, HarnessPayload};
pub use event_id::EventId;
pub use filter::Filter;
pub use lifecycle::{HarnessSource, LifecycleEvent};
#[cfg(all(unix, feature = "uds"))]
pub use push_client::{DEFAULT_PUSH_BUFFER_CAPACITY, PushClient, PushFlushOutcome};

#[cfg(test)]
mod tests;
