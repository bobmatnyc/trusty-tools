//! Workstream-level SSE event aggregation (DOC-48 §5.3/§5.3.1, issue #3297).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft) §5.3
//! - [`SPEC-WS-07~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-07~draft) AC-7
//!
//! Why: `GET /workstreams/{id}/events` (`crate::serve::rest::workstream_events`)
//! must present a workstream as ONE logical SSE stream even though the daemon
//! only tracks events per-session (`crate::events`, session-keyed per
//! AC-7.3). §5.3 requires fanning out over the workstream's bound
//! `session_ids`, tagging each event `{session_id, event_type, payload}`, and
//! merging in workstream-level events (`WorkstreamActivationChanged`,
//! `WorkstreamStateInferred`) that have no single owning session. AC-7.1
//! additionally requires this fan-out layer to be designed against a
//! session-id ABSTRACTION rather than `crate::session::SessionRegistry`
//! directly, so it can be lifted into `trusty-agents-common` (#3299) for
//! `trusty-agents` background sessions (epic #3052) without rework — this
//! module is that seam: [`SessionEventSource`] is the trait a tcode-native
//! adapter ([`RegistrySessionEventSource`]) implements today, and a future
//! tagents-native adapter will implement identically once extracted.
//!
//! What: [`WorkstreamEventEnvelope`] (the generic `{session_id, event_type,
//! payload}` wire shape, AC-7.2), [`SessionEventSource`] (the harness-agnostic
//! per-session stream seam, AC-7.1) plus its tcode-native impl
//! [`RegistrySessionEventSource`], [`aggregate`] (the merge function
//! `crate::serve::rest::workstream_events` calls — one stream per bound
//! session plus the workstream-level bus, `futures_util::stream::select_all`),
//! and the workstream-level event bus
//! ([`publish_activation_changed`]/[`publish_state_inferred`]/
//! [`subscribe_workstream_bus`]) that [`super::activation`] and
//! [`super::protocol`] publish onto when the activation-lock or close verbs
//! fire a state transition.
//!
//! **Dynamic membership (§5.3 step 4, resolved ambiguity):** this ticket
//! implements STATIC-at-subscribe-time fan-out — [`aggregate`] snapshots
//! `session_ids` once, at the moment `GET /workstreams/{id}/events` is called,
//! and does not notice a session bound to the workstream afterward. Session
//! BINDING itself (`session.create(workstream_id)`) is #3298 scope and does
//! not exist yet — `Workstream::session_ids` never grows today (see
//! `super::model` — nothing currently calls `WorkstreamStore` to append an
//! id), so there is no live membership-change event to react to yet. When
//! #3298 lands session binding, it should also publish onto
//! [`subscribe_workstream_bus`] (an event this module's aggregator already
//! merges in) so a follow-up can make membership changes dynamic without
//! touching this file's merge logic again — noted here as the deliberate
//! seam for that follow-up.
//!
//! **`SessionAdded`/`SessionActivityUpdate` deferred:** DOC-48 §13's event
//! list also names these two, but both describe session-BINDING activity
//! (`{session_id, binding_time}` / activity/heartbeat on a bound session) —
//! there is no producer for either today because #3298 (session binding) has
//! not landed, exactly like the dynamic-membership note above. Wiring them in
//! now would mean inventing call sites this crate does not yet have (no code
//! path ever adds to `session_ids`). [`WorkstreamEventEnvelope`]'s shape
//! already accommodates them without a wire-format change once #3298 lands.
//!
//! Test: `events_tests`.

use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use futures_util::stream::{self, Stream, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use super::model::{WorkstreamId, WorkstreamState};
use crate::session::SessionRegistry;

/// A boxed, owned, `'static` event stream — the common currency every piece
/// of this module passes around (per-session streams, the workstream-level
/// bus, and the final merged stream all share this one type).
pub type EventStream = Pin<Box<dyn Stream<Item = WorkstreamEventEnvelope> + Send>>;

/// The generic wire envelope `GET /workstreams/{id}/events` emits (DOC-48
/// §5.3 step 3, AC-7.2): `{session_id, event_type, payload}`.
///
/// Why: AC-7.2 requires a harness-agnostic envelope shape, not tcode's
/// session-scoped [`crate::events::SessionEventEnvelope`] (which always
/// carries a `session_id` and a closed-set `Event` enum) — a
/// workstream-level event like `WorkstreamActivationChanged` has no single
/// owning session, so `session_id` must be optional here.
/// What: `session_id` is `Some(id)` for an event fanned out from one of the
/// workstream's bound sessions, `None` for a workstream-level event.
/// `event_type` is the event's stable snake_case tag (mirrors
/// `crate::events::Event::kind`'s convention). `payload` is the
/// event-specific JSON body.
/// Test: `events_tests::session_envelope_carries_session_id`,
/// `events_tests::workstream_level_envelope_has_no_session_id`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkstreamEventEnvelope {
    pub session_id: Option<String>,
    pub event_type: String,
    pub payload: Value,
}

impl WorkstreamEventEnvelope {
    /// Build an envelope for an event fanned out from a specific bound
    /// session.
    fn for_session(
        session_id: impl Into<String>,
        event_type: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self {
            session_id: Some(session_id.into()),
            event_type: event_type.into(),
            payload,
        }
    }

    /// Build an envelope for a workstream-level event with no owning
    /// session (`WorkstreamActivationChanged`, `WorkstreamStateInferred`).
    fn workstream_level(event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            session_id: None,
            event_type: event_type.into(),
            payload,
        }
    }
}

/// AC-7.1's harness-agnostic seam: the aggregation layer's only dependency
/// on "how does a session emit events", abstracted behind a generic
/// `session_id: &str` rather than `crate::session::SessionRegistry`.
///
/// Why: this is the exact interface boundary DOC-48 §5.3.1 requires be
/// extractable to `trusty-agents-common` (#3299) — [`aggregate`] (and the
/// SSE route that calls it) never touch `SessionRegistry` directly, only
/// this trait, so lifting the aggregation layer into a shared crate is a
/// matter of moving [`aggregate`]/[`WorkstreamEventEnvelope`]/this trait
/// verbatim and leaving [`RegistrySessionEventSource`] behind as tcode's
/// local adapter (a future `trusty-agents`-native adapter implements the
/// same trait over its own session/event types).
/// What: [`Self::subscribe`] returns this session's live event stream
/// already tagged as [`WorkstreamEventEnvelope`]s, or `None` if `session_id`
/// names no session the source knows about (a dangling/removed binding —
/// the aggregator skips it rather than failing the whole request, since one
/// stale bound session must never take down observation of the rest).
/// Test: `events_tests::aggregate_merges_per_session_and_workstream_streams`.
pub trait SessionEventSource: Send + Sync {
    fn subscribe(&self, session_id: &str) -> Option<EventStream>;
}

/// The tcode-native [`SessionEventSource`] impl, wrapping the daemon's real
/// [`SessionRegistry`] + global event bus (`crate::events`).
///
/// Why: the one place this module touches tcode-specific types — every
/// other item here (the trait, the envelope, [`aggregate`]) is generic.
/// What: [`Self::subscribe`] replays a session's ring buffer
/// ([`SessionRegistry::replay`]) then chains its live events, filtered from
/// the shared global bus by `session_id` — the exact same replay-then-live
/// composition `crate::serve::http::session_events_sse` already uses for the
/// per-session route, reused here rather than reimplemented.
/// Test: `events_tests::registry_source_replays_then_streams_live_events`,
/// `events_tests::registry_source_unknown_session_returns_none`.
pub struct RegistrySessionEventSource {
    sessions: Arc<SessionRegistry>,
}

impl RegistrySessionEventSource {
    pub fn new(sessions: Arc<SessionRegistry>) -> Self {
        Self { sessions }
    }
}

impl SessionEventSource for RegistrySessionEventSource {
    fn subscribe(&self, session_id: &str) -> Option<EventStream> {
        let replay = self.sessions.replay(session_id).ok()?;
        let replay_stream = stream::iter(replay.into_iter().map(session_envelope_to_workstream));

        let filter_id = session_id.to_string();
        let live_stream =
            BroadcastStream::new(crate::events::subscribe()).filter_map(move |item| {
                let filter_id = filter_id.clone();
                async move {
                    match item {
                        Ok(envelope) if envelope.session_id == filter_id => {
                            Some(session_envelope_to_workstream(envelope))
                        }
                        _ => None,
                    }
                }
            });

        Some(Box::pin(replay_stream.chain(live_stream)))
    }
}

/// Adapt one `crate::events::SessionEventEnvelope` into the generic
/// [`WorkstreamEventEnvelope`] shape (`event_type` == `Event::kind()`,
/// `payload` == the serialized `Event`).
fn session_envelope_to_workstream(
    envelope: crate::events::SessionEventEnvelope,
) -> WorkstreamEventEnvelope {
    let payload = serde_json::to_value(&envelope.event).unwrap_or(Value::Null);
    WorkstreamEventEnvelope::for_session(envelope.session_id, envelope.kind, payload)
}

/// Merge each bound session's event stream (via `source`) with the
/// workstream-level stream into one [`EventStream`] (DOC-48 §5.3 steps 1-3;
/// AC-7.1's aggregation layer).
///
/// Why: the single function `crate::serve::rest::workstream_events` calls —
/// keeping the merge logic here (rather than inline in the axum handler)
/// is what makes it independently unit-testable against a fake
/// [`SessionEventSource`] and, per the module docs, independently liftable
/// into `trusty-agents-common`.
/// What: `session_ids` is snapshotted by the caller ONCE (static-at-subscribe
/// -time membership, see module docs) — an unknown/dangling id is silently
/// skipped (`SessionEventSource::subscribe` returning `None`), never fatal to
/// the whole aggregation. An empty `session_ids` still returns a live stream
/// containing just `workstream_events` — matching DOC-48 §5.3's "zero-session
/// workstream: stream stays open, workstream-level events only" requirement.
/// Test: `events_tests::aggregate_merges_per_session_and_workstream_streams`,
/// `events_tests::aggregate_with_no_sessions_still_streams_workstream_events`,
/// `events_tests::aggregate_skips_unknown_session_ids`.
pub fn aggregate(
    source: &dyn SessionEventSource,
    session_ids: &[String],
    workstream_events: EventStream,
) -> EventStream {
    let mut streams: Vec<EventStream> = session_ids
        .iter()
        .filter_map(|id| source.subscribe(id))
        .collect();
    streams.push(workstream_events);
    Box::pin(stream::select_all(streams))
}

/// Capacity of the workstream-level event bus. Far lower volume than
/// `crate::events`'s per-session bus (`CHANNEL_CAPACITY` 1024) — activation
/// changes and state transitions are rare, daemon-wide occurrences, not a
/// per-turn stream.
const WORKSTREAM_BUS_CAPACITY: usize = 256;

/// Process-global bus for workstream-LEVEL events (`WorkstreamActivationChanged`,
/// `WorkstreamStateInferred`) — deliberately separate from `crate::events`'s
/// per-session bus, since these events have no owning `session_id` (AC-3.3:
/// "ALL connected clients receive `WorkstreamActivationChanged`", not just
/// those observing the affected workstream(s) — so no per-workstream
/// filtering is applied on publish; every subscriber of every
/// `/workstreams/{id}/events` stream receives every event on this bus).
static WORKSTREAM_BUS: OnceLock<broadcast::Sender<WorkstreamEventEnvelope>> = OnceLock::new();

fn workstream_bus() -> broadcast::Sender<WorkstreamEventEnvelope> {
    WORKSTREAM_BUS
        .get_or_init(|| broadcast::channel(WORKSTREAM_BUS_CAPACITY).0)
        .clone()
}

/// Subscribe to the workstream-level event bus as an [`EventStream`], ready
/// to hand to [`aggregate`].
///
/// Test: `events_tests::workstream_bus_round_trips_through_subscribe`.
pub fn subscribe_workstream_bus() -> EventStream {
    Box::pin(
        BroadcastStream::new(workstream_bus().subscribe())
            .filter_map(|item| async move { item.ok() }),
    )
}

/// Publish `WorkstreamActivationChanged` (DOC-48 §5.3, AC-3.3) — called by
/// [`super::activation::activate`]/[`super::activation::deactivate`] whenever
/// the daemon-wide active pointer actually changes.
///
/// Why: the single production call site for this event kind, so its wire
/// shape (`{new_active_id, prior_id}`) can never drift between call sites.
/// Test: `events_tests::publish_activation_changed_reaches_subscriber`.
pub fn publish_activation_changed(new_active_id: WorkstreamId, prior_id: Option<WorkstreamId>) {
    let envelope = WorkstreamEventEnvelope::workstream_level(
        "workstream_activation_changed",
        json!({ "new_active_id": new_active_id, "prior_id": prior_id }),
    );
    let _ = workstream_bus().send(envelope);
}

/// Publish `WorkstreamStateInferred` (DOC-48 §5.3) — called whenever a
/// workstream's computed state actually transitions (activated, deactivated,
/// or closed).
///
/// Why: mirrors [`publish_activation_changed`] — one call site per event
/// kind, so the wire shape (`{workstream_id, state, reason}`) stays
/// consistent.
/// Test: `events_tests::publish_state_inferred_reaches_subscriber`.
pub fn publish_state_inferred(workstream_id: WorkstreamId, state: WorkstreamState, reason: &str) {
    let envelope = WorkstreamEventEnvelope::workstream_level(
        "workstream_state_inferred",
        json!({ "workstream_id": workstream_id, "state": state, "reason": reason }),
    );
    let _ = workstream_bus().send(envelope);
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;
