//! Workstream-level event aggregation (DOC-48 §5.3, §5.3.1; issue #3297,
//! epic #3292); thin `trusty-agents-common::transport` adapter (issue #3299,
//! Phase 1B); transport-free since #6637 PR 2c.
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft) §5.3, §5.3.1
//! - [`SPEC-WS-07~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-07~draft) AC-7
//!
//! Why: `session.events` (`crate::session::events_stream`) is a direct lookup
//! against ONE session's ring buffer + live filter. A
//! workstream is a GROUP of sessions (§2.1), so observing "this workstream"
//! is not a lookup at all — it is a fan-out: subscribe once to the daemon's
//! existing session-scoped event bus (`crate::events::subscribe`, already
//! shared by every per-session tail) and forward only the events
//! whose `session_id` is currently bound to this workstream, tagged with the
//! generic `{session_id, event_type, payload}` envelope AC-7.2 requires.
//! "Currently bound" is re-checked against the store on every event (not a
//! snapshot taken at subscribe time) so a session added to the workstream
//! after the client connected is picked up without a reconnect (§5.3 point
//! 4) — cheap in practice because `WorkstreamStore::get` only re-parses the
//! backing file when its (mtime, len) fingerprint actually changed.
//!
//! **AC-7 harness-agnostic seam (Phase 1B, issue #3299):** the fan-out
//! algorithm itself now lives in
//! [`trusty_agents_common::transport::aggregate_live`] — this module is a
//! thin adapter that implements the shared `EventSource`/`MembershipProvider`
//! traits over `crate::events`/`SharedWorkstreamStore` and supplies the
//! `classify` closure encoding tcode's two group-scoped bypass events
//! (`WorkstreamActivationChanged`, `WorkstreamStateInferred`). Behavior is
//! byte-for-byte identical to the Phase 1A implementation this replaces (see
//! `sse_tests`, ported unchanged); only the fan-out combinator itself moved.
//!
//! What: [`WorkstreamEventEnvelope`] (a type alias over the shared
//! `EventEnvelope<Event>`), [`aggregate_live`] (this module's adapter
//! function — constructs the two trait impls and the `classify` closure,
//! then delegates to the shared combinator), and [`map_store_err`].
//!
//! **This module binds nothing (#6637 PR 2c).** It used to carry the
//! `GET /workstreams/{id}/events` axum route as well; that route retired with
//! the daemon's TCP listener. `crate::workstreams::events_stream` is the one
//! caller now — it registers the same fan-out as the `workstream.events`
//! stream method and owns the id validation the route used to do, so the
//! refusals (`invalid_params` for a non-UUID, `not_found` for an unknown id)
//! are unchanged and a CLOSED workstream stays observable (§4.4). A
//! workstream with zero bound sessions yields a stream that simply never
//! emits, rather than an error.
//! Test: `sse_tests`.

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;
use trusty_agents_common::transport::{
    BoxEventStream, EventSource, MembershipProvider, SourceEvent,
};

use crate::events::Event;
use crate::jsonrpc::RpcError;

use super::activation::SharedWorkstreamStore;
use super::model::WorkstreamId;
use super::store::StoreError;

/// The harness-agnostic wire envelope AC-7.2 requires: `{session_id,
/// event_type, payload}` — a type alias over
/// `trusty_agents_common::transport::EventEnvelope<Event>` (Phase 1B, issue
/// #3299), deliberately distinct from `crate::events::SessionEventEnvelope`
/// (which carries `seq`/`at`/`kind`/`event`, tuned for one session's
/// ring-buffer replay).
pub type WorkstreamEventEnvelope = trusty_agents_common::transport::EventEnvelope<Event>;

/// [`EventSource`] adapter over `crate::events` — subscribes to the SAME
/// daemon-global broadcast bus every per-session SSE connection already
/// reads, wrapping each envelope into the shared crate's
/// session-id/event-type/payload triple.
struct TcodeEventSource;

impl EventSource for TcodeEventSource {
    type Payload = Event;

    fn subscribe(&self) -> BoxEventStream<Event> {
        Box::pin(
            BroadcastStream::new(crate::events::subscribe()).filter_map(|item| async move {
                item.ok().map(|envelope| SourceEvent {
                    session_id: envelope.session_id.clone(),
                    event_type: envelope.kind.clone(),
                    payload: envelope.event.clone(),
                })
            }),
        )
    }
}

/// [`MembershipProvider`] adapter over [`SharedWorkstreamStore`] — "is this
/// session currently in `workstream_id`'s `session_ids`" re-fetched from the
/// store on every call (never a stale snapshot), matching §5.3's dynamic
/// membership requirement. A store lookup failure (the workstream vanished
/// from under an open connection — not reachable via any current mutation
/// path, since `workstream.close` never removes a record) is treated as
/// "no match" per this trait's documented contract, matching this route's
/// "stay open" bias for a zero-session workstream.
#[derive(Clone)]
struct WorkstreamMembership(SharedWorkstreamStore);

#[async_trait]
impl MembershipProvider<WorkstreamId> for WorkstreamMembership {
    async fn contains(&self, group: &WorkstreamId, session_id: &str) -> bool {
        self.0
            .lock()
            .await
            .get(*group)
            .await
            .map(|ws| ws.session_ids.iter().any(|s| s == session_id))
            .unwrap_or(false)
    }
}

/// The `classify` bypass: forwards `WorkstreamActivationChanged` iff it
/// names `target` as its `new_active_id` OR `prior_id` (a workstream
/// observer must learn when IT stops being active, not only when a session
/// it owns emits, §6.3), and `WorkstreamStateInferred` iff its
/// `workstream_id` names `target` (an observer must learn its own state
/// changed — e.g. it was closed — even with zero bound sessions). Any other
/// event falls through to [`WorkstreamMembership`] (`None`). `target` is
/// `&str` (not `&WorkstreamId`) so [`aggregate_live`] can stringify the id
/// ONCE at stream construction rather than on every event flowing over the
/// daemon-global bus.
fn classify(event: &Event, target: &str) -> Option<bool> {
    match event {
        Event::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => Some(new_active_id.as_deref() == Some(target) || prior_id.as_deref() == Some(target)),
        Event::WorkstreamStateInferred { workstream_id, .. } => {
            Some(workstream_id.as_str() == target)
        }
        _ => None,
    }
}

/// Build the live aggregation stream for one workstream (no ring-buffer
/// replay — see module docs).
///
/// Why: the single seam [`crate::workstreams::events_stream::open`] drives;
/// kept as a free function so a unit test can exercise the fan-out logic
/// directly against a `SharedWorkstreamStore`, without opening a socket.
/// As of Phase 1B (issue #3299) this is a thin adapter over
/// [`trusty_agents_common::transport::aggregate_live`] — see this module's
/// docs for the trait impls it supplies.
/// What: delegates to the shared combinator with [`TcodeEventSource`],
/// [`WorkstreamMembership`], and the [`classify`] bypass closure.
/// Test: `sse_tests::fan_out_tags_events_from_bound_sessions_only`,
/// `sse_tests::activation_changed_event_is_forwarded_regardless_of_session_binding`,
/// `sse_tests::state_inferred_event_is_forwarded_for_this_workstream_only`,
/// `sse_tests::empty_workstream_stream_yields_nothing`.
pub fn aggregate_live(
    workstream_id: WorkstreamId,
    store: SharedWorkstreamStore,
) -> impl Stream<Item = WorkstreamEventEnvelope> {
    // Stringified ONCE here (not per event): the classify closure runs for
    // every event on the daemon-global bus, so it must not re-allocate the
    // target id each time.
    let target = workstream_id.to_string();
    trusty_agents_common::transport::aggregate_live(
        workstream_id,
        TcodeEventSource,
        WorkstreamMembership(store),
        move |event: &Event, _group: &WorkstreamId| classify(event, &target),
    )
}

/// Map a [`StoreError`] onto the JSON-RPC error taxonomy (mirrors
/// `crate::workstreams::protocol::map_store_err`, kept separate since that
/// function is private to `protocol`).
///
/// `pub(super)` since #6637: `crate::workstreams::events_stream` answers the
/// same two refusals over the socket transport and must report the same codes.
pub(super) fn map_store_err(err: StoreError) -> RpcError {
    match err {
        StoreError::NotFound(id) => RpcError::not_found(format!("workstream not found: {id}")),
        StoreError::Io(_) | StoreError::Serialize(_) => RpcError::internal(err.to_string()),
    }
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod sse_tests;
