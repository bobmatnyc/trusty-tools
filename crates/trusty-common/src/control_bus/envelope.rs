//! The `HarnessEvent` envelope and its domain-tagged `HarnessPayload` union.
//!
//! Why: Producers stamp events here and trusty-console consumes them there, so
//!      the envelope has to be a type both sides link against. Carrying
//!      `source`, `session`, `seq` and `at` on the envelope keeps each domain's
//!      payload taxonomy free of repeated bookkeeping fields, and lets a
//!      subscriber order, correlate and route an event without decoding what is
//!      inside it.
//! What: Defines `HarnessPayload` (the domain-tagged inner union) and
//!       `HarnessEvent` (the envelope). Types only — the transport that fills
//!       `seq` and `at`, and the channel these travel over, belong to whoever
//!       owns the bus.
//! Test: `super::tests::harness_event_round_trips`,
//!       `super::tests::harness_event_omits_none_session`,
//!       `super::tests::payload_lifecycle_round_trips`,
//!       `super::tests::payload_hook_round_trips`,
//!       `super::tests::payload_ping_round_trips`,
//!       `super::tests::payload_domain_matches_serde_tag`.

// #6846: `HarnessPayload` and `HarnessEvent` moved here from
// `trusty_agents_common::events::bus`; that module keeps its stderr transport.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::lifecycle::{HarnessSource, LifecycleEvent};

/// Domain-tagged inner union carried inside a `HarnessEvent`.
///
/// Why: The "adapt, don't fold" decision (ADR-0005): rather than flattening
///      lifecycle, hook, and keepalive events into one giant enum, we tag by
///      *domain* so each harness can grow its own payload taxonomy
///      independently. Hooks in particular are open-ended (arbitrary
///      tool/event names + JSON data), so they get an untyped `Value` arm
///      instead of being modelled variant-by-variant.
/// What: `serde(tag = "domain", content = "event")` produces
///       `{"domain":"lifecycle","event":{...}}`, `{"domain":"hook","event":
///       {"kind":...,"data":...}}`, or `{"domain":"ping"}`. `Ping` is the
///       transport keepalive, kept out of the lifecycle enum.
/// Test: `super::tests::payload_lifecycle_round_trips`,
///       `super::tests::payload_hook_round_trips`,
///       `super::tests::payload_ping_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "domain", content = "event", rename_all = "snake_case")]
pub enum HarnessPayload {
    /// A structured PM-lifecycle event (session/agent/tool/phase/LLM/...).
    Lifecycle(LifecycleEvent),
    /// An open-ended hook event: `kind` names the hook, `data` is its payload.
    Hook { kind: String, data: Value },
    /// Transport keepalive so long-lived SSE connections don't time out.
    Ping,
}

impl HarnessPayload {
    /// The domain string for this payload (`"lifecycle"`, `"hook"`, `"ping"`).
    ///
    /// Why: `Filter` matches on the domain without serialising the whole
    ///      payload; keeping the mapping here is the single source of truth.
    /// What: Returns the same string serde uses for the `domain` tag.
    /// Test: `super::tests::payload_domain_matches_serde_tag`.
    pub fn domain(&self) -> &'static str {
        match self {
            HarnessPayload::Lifecycle(_) => "lifecycle",
            HarnessPayload::Hook { .. } => "hook",
            HarnessPayload::Ping => "ping",
        }
    }
}

/// Cross-harness event envelope: metadata plus domain-tagged payload.
///
/// Why: Subscribers order (`seq`), time-stamp (`at`), attribute (`source`) and
///      correlate (`session`) events uniformly, whichever harness produced them
///      and whichever domain the payload belongs to. Holding that metadata on
///      the envelope keeps the payload taxonomies free of repeated bookkeeping
///      fields.
/// What: `source` is the originating harness; `session` is the optional task
///       correlation key (omitted from JSON when `None`); `seq` is a
///       process-monotonic counter assigned by the producer; `at` is the
///       emit-time UTC timestamp; `payload` is the domain-tagged union. All
///       fields are public so a producer can stamp one by struct literal.
/// Test: `super::tests::harness_event_round_trips` and
///       `super::tests::harness_event_omits_none_session`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HarnessEvent {
    /// Which harness produced this event.
    pub source: HarnessSource,
    /// Optional task/session correlation key. Omitted from JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Process-monotonic sequence number assigned at publish time.
    pub seq: u64,
    /// Emit-time UTC timestamp.
    pub at: DateTime<Utc>,
    /// Domain-tagged payload.
    pub payload: HarnessPayload,
}
