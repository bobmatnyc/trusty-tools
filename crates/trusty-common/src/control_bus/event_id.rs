//! `EventId`: a UUIDv7 identifier minted once by the emitting process.
//!
//! Why: `HarnessEvent.seq` is process-monotonic (per-session in trusty-code),
//!      so two producers stamp colliding values, and it carries no causal
//!      information. The dashboard's tree view (DOC-73 §5.2) needs an
//!      identifier that stays globally unique and stable through every relay
//!      hop, so events arriving out of order or over different transports
//!      still assemble into one call graph (issue #6847, DOC-73 §3.1).
//! What: A newtype over `uuid::Uuid`. `EventId::new` mints a UUIDv7 —
//!       time-ordered, so ids sort close to `seq`/`at` order even before a
//!       central bus assigns a total order. Serializes as the standard
//!       hyphenated UUID string via `#[serde(transparent)]`. Note: a UUIDv7's
//!       embedded millisecond timestamp is redundant with `HarnessEvent.at`
//!       (the envelope's own, explicitly-stamped time) — readers needing an
//!       event's time should use `at`, not decode the id. The id's
//!       time-ordering is a sort-friendliness property, not a second
//!       timestamp source.
//! Test: `super::tests::event_id_round_trips`,
//!       `super::tests::event_id_new_mints_distinct_ids`,
//!       `super::tests::event_id_display_matches_serialized_string`.

// #6847: minting is a plain constructor callers invoke at emit time (DOC-73
// §3.1 — "id is a UUIDv7 minted by the emitting process"), never a bus or
// transport concern; `control_bus_declares_no_transport` in `tests.rs` holds
// this file to the same types-only boundary as the rest of the module.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Globally-unique, transport-stable identifier for one `HarnessEvent`.
///
/// Why: See module docs — `seq` cannot serve as cross-process identity.
/// What: Wraps a `uuid::Uuid`. `Copy`/`Eq`/`Hash` so it is cheap to use as a
///       map key (the tree view indexes nodes by `id`).
/// Test: `super::tests::event_id_round_trips`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(Uuid);

impl EventId {
    /// Mint a fresh UUIDv7 id.
    ///
    /// Why: Called once, by the emitting process, at publish time (DOC-73
    ///      §3.1 / §4.2) — the bus and every relay hop pass the value through
    ///      unchanged rather than re-minting it.
    /// What: `Uuid::now_v7()`, wrapped. Also backs `#[serde(default)]` on
    ///       `HarnessEvent.id` (via `Default`, below), so a legacy payload
    ///       serialized before this field existed still deserializes.
    /// Test: `super::tests::event_id_new_mints_distinct_ids`.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}
