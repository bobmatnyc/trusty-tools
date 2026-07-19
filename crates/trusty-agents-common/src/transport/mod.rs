//! Shared multi-client attach/fan-out transport (DOC-48 §5.3.1, AC-7; issue
//! #3299, epic #3292; twin epic #3052).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft) §5.3.1
//! - [`SPEC-WS-07~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-07~draft) AC-7
//!
//! Why: DOC-48 §5.3.1 designates the multi-client SSE fan-out + per-session
//! event tagging used by tcode's workstream observation endpoint as
//! **harness-agnostic** — the same shape `trusty-agents` background sessions
//! need for epic #3052 (iOS thin client over VPN). Phase 1A
//! (`trusty-code::workstreams::sse`, issue #3297) proved the fan-out
//! algorithm against a concrete harness-private event type while keeping the
//! seam free of tcode-specific session types (only plain `String` session
//! ids and a `WorkstreamId`-shaped generic group id). This module is that
//! seam's landing zone: the same algorithm, generalised over an opaque group
//! id and an opaque event payload, with zero `axum`/tcode/HTTP dependency —
//! HTTP framing stays in each consumer (`trusty-code::workstreams::sse`
//! keeps its `axum::Router`; a future `trusty-agents` adopter keeps its own
//! transport).
//! What: [`SourceEvent`]/[`EventEnvelope`] (the AC-7.2 wire shape — a source
//! event and its forwarded counterpart share the same three fields, so one
//! type serves both; kept as a distinct alias-free struct with `#[derive]`
//! rather than reusing a tcode-specific ring-buffer envelope, per §5.3.1's
//! "generic envelope, no domain event coupling" requirement),
//! [`EventSource`] (a group-agnostic subscription trait — "subscribe once,
//! get a stream of session-tagged events"), [`MembershipProvider`] (a
//! group-membership check trait — "does this group currently contain this
//! session id", async so a store-backed implementation can await a lock),
//! and [`aggregate_live`] (the fan-out combinator: forwards an event
//! unconditionally per an optional `classify` bypass, or falls through to a
//! `MembershipProvider` lookup — this is the exact two-path decision
//! `trusty-code::workstreams::sse::aggregate_live` made inline, factored out
//! so `classify`'s domain-specific bypass rules — e.g. tcode's
//! `WorkstreamActivationChanged`/`WorkstreamStateInferred` — live entirely
//! in the caller, never in this crate).
//! Test: `transport_tests` exercises the combinator against an in-memory
//! `EventSource`/`MembershipProvider` pair; `trusty-code`'s own
//! `sse_tests::fan_out_tags_events_from_bound_sessions_only` and siblings
//! remain the end-to-end regression guard for the tcode adapter built on top.

use std::pin::Pin;

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use serde::Serialize;

/// One event as it arrives from an [`EventSource`], before this module
/// decides whether to forward it.
///
/// Why: kept structurally identical to [`EventEnvelope`] (not merged into
/// one type) so a future `EventSource` impl that needs to distinguish
/// "received" from "forwarded" (e.g. for metrics) has a seam to hook into
/// without changing the wire type. `event_type` is a plain `String` (not an
/// enum) because it is meant to mirror the payload's own `kind()`-style
/// stable tag — the same convention `trusty-code`'s `SessionEventEnvelope`
/// already uses — without this crate needing to know the payload type's
/// shape.
/// Test: `transport_tests::forwards_event_when_membership_matches`.
#[derive(Debug, Clone)]
pub struct SourceEvent<P> {
    pub session_id: String,
    pub event_type: String,
    pub payload: P,
}

/// The AC-7.2 wire envelope: `{session_id, event_type, payload}`.
///
/// Why: this is the literal shape AC-7.2 mandates for any harness-agnostic
/// SSE/attach consumer, generic over the payload type so neither tcode's
/// `Event` enum nor a future `trusty-agents` event type need be known here.
/// What: `session_id` is empty for a group-scoped event with no owning
/// session (mirrors `trusty-code::workstreams::sse::WorkstreamEventEnvelope`'s
/// `daemon_scoped` convention — that convention is a caller concern, not
/// enforced by this type).
/// Test: `transport_tests::forwards_event_when_membership_matches`.
#[derive(Debug, Clone, Serialize)]
pub struct EventEnvelope<P> {
    pub session_id: String,
    pub event_type: String,
    pub payload: P,
}

impl<P> From<SourceEvent<P>> for EventEnvelope<P> {
    fn from(src: SourceEvent<P>) -> Self {
        Self {
            session_id: src.session_id,
            event_type: src.event_type,
            payload: src.payload,
        }
    }
}

/// A boxed, owned, `Send` stream — the shape [`EventSource::subscribe`]
/// returns, avoiding an associated-type-with-impl-Trait requirement on
/// implementors.
pub type BoxEventStream<P> = Pin<Box<dyn Stream<Item = SourceEvent<P>> + Send>>;

/// The event-source seam: "subscribe once, get a stream of session-tagged
/// events."
///
/// Why: AC-7.1 requires the interface be "a trait ... `trusty-agents` can
/// implement without rework" — this trait names no concrete transport
/// (broadcast channel, SSE client, in-memory queue); an implementor owns
/// however it produces the stream. `Payload` is the implementor's own event
/// type (tcode's `Event` enum today; a `trusty-agents` harness event type
/// tomorrow).
/// What: one method, `subscribe`, called once per logical attach
/// (`aggregate_live` calls it exactly once — matching Phase 1A's "subscribe
/// ONCE to the daemon-global bus" design, not once per group member).
/// Test: `transport_tests` implements this over a `tokio::sync::broadcast`
/// channel, the same primitive `trusty-code`'s adapter uses.
pub trait EventSource: Send + Sync {
    type Payload: Send + 'static;

    fn subscribe(&self) -> BoxEventStream<Self::Payload>;
}

/// The group-membership seam: "does this group currently contain this
/// session id, right now."
///
/// Why: AC-7.3/§5.3.1 require the aggregation layer to re-check membership
/// live (not a snapshot taken at subscribe time), so a session added to a
/// group after a client attaches is picked up without a reconnect. `async`
/// so a store-backed implementation (tcode's `SharedWorkstreamStore`,
/// behind a `tokio::sync::Mutex`) can await its lock without this crate
/// knowing that detail.
/// What: `Group` is the implementor's own group-id type (tcode's
/// `WorkstreamId`; a future `trusty-agents` "logical unit" id for epic
/// #3052). `contains` returns `false` (never errors) when the group cannot
/// be resolved — mirrors Phase 1A's "a lookup failure is 'no match', not a
/// closed stream" bias, kept as this trait's documented contract so every
/// implementor behaves the same way.
/// Test: `transport_tests::membership_lookup_failure_is_treated_as_no_match`.
#[async_trait]
pub trait MembershipProvider<Group>: Send + Sync {
    async fn contains(&self, group: &Group, session_id: &str) -> bool;
}

/// Build the live fan-out stream for one group.
///
/// Why: kept as a free function (not a method on a struct) so it stays
/// trivially unit-testable against an in-memory `EventSource`/
/// `MembershipProvider` pair, mirroring why
/// `trusty-code::workstreams::sse::aggregate_live` was a free function
/// before this extraction.
/// What: subscribes ONCE via `source.subscribe()`. For each event: if
/// `classify(payload, group)` returns `Some(bypass)`, the membership check
/// is skipped entirely and the event is forwarded iff `bypass` — this is
/// the seam a caller uses for group-scoped events that carry no session id
/// at all (tcode's `WorkstreamActivationChanged`/`WorkstreamStateInferred`).
/// If `classify` returns `None`, the event is forwarded iff
/// `membership.contains(group, session_id)` is `true` at THAT moment (not a
/// stale snapshot — re-checked on every event, per §5.3.1's "dynamic
/// membership" requirement).
/// Test: `transport_tests::forwards_event_when_membership_matches`,
/// `transport_tests::filters_event_when_membership_does_not_match`,
/// `transport_tests::classify_bypass_skips_membership_check`,
/// `transport_tests::membership_lookup_failure_is_treated_as_no_match`.
pub fn aggregate_live<Group, Src, Mem, Classify>(
    group: Group,
    source: Src,
    membership: Mem,
    classify: Classify,
) -> impl Stream<Item = EventEnvelope<Src::Payload>>
where
    Group: Clone + Send + Sync + 'static,
    Src: EventSource,
    Mem: MembershipProvider<Group> + Clone + Send + Sync + 'static,
    Classify: Fn(&Src::Payload, &Group) -> Option<bool> + Send + Sync + 'static,
{
    source.subscribe().filter_map(move |item| {
        let group = group.clone();
        let membership = membership.clone();
        let bypass = classify(&item.payload, &group);
        async move {
            match bypass {
                Some(true) => Some(EventEnvelope::from(item)),
                Some(false) => None,
                None => membership
                    .contains(&group, &item.session_id)
                    .await
                    .then(|| EventEnvelope::from(item)),
            }
        }
    })
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
