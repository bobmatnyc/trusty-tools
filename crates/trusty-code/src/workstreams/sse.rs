//! Workstream-level SSE event aggregation route (DOC-48 §5.3, §5.3.1; issue
//! #3297, epic #3292); thin `trusty-agents-common::transport` adapter (issue
//! #3299, Phase 1B).
//!
//! # Spec References
//!
//! - [`SPEC-WS-05~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-05~draft) §5.3, §5.3.1
//! - [`SPEC-WS-07~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-07~draft) AC-7
//!
//! Why: `GET /sessions/{id}/events` (`crate::serve::http::session_events_sse`)
//! is a direct lookup against ONE session's ring buffer + live filter. A
//! workstream is a GROUP of sessions (§2.1), so observing "this workstream"
//! is not a lookup at all — it is a fan-out: subscribe once to the daemon's
//! existing session-scoped event bus (`crate::events::subscribe`, already
//! shared by every per-session SSE connection) and forward only the events
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
//! then delegates to the shared combinator), and [`routes`] (the single
//! `GET /workstreams/{id}/events` axum route, merged into
//! `crate::serve::http::build_axum_router` alongside `rest::workstreams`).
//! Unknown workstream id -> `404`; malformed (non-UUID) id -> `400`; a
//! CLOSED workstream is still observable (`200`, not `404`) because its
//! existing bound sessions "remain valid and may accumulate turns" after
//! closure (§4.4) — closing only stops NEW session bindings, not event
//! delivery for the ones it already has. A workstream with zero bound
//! sessions returns `200` with a stream that simply never emits (until a
//! future session is added or the workstream's activation state changes) —
//! it does not error or close early.
//! Test: `sse_tests`.

use std::convert::Infallible;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router as AxumRouter};
use futures_util::{Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;
use trusty_agents_common::transport::{
    BoxEventStream, EventSource, MembershipProvider, SourceEvent,
};
use trusty_common::mcp::Response;
use uuid::Uuid;

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
/// event falls through to [`WorkstreamMembership`] (`None`).
fn classify(event: &Event, target: &WorkstreamId) -> Option<bool> {
    let target = target.to_string();
    match event {
        Event::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } => Some(
            new_active_id.as_deref() == Some(target.as_str())
                || prior_id.as_deref() == Some(target.as_str()),
        ),
        Event::WorkstreamStateInferred { workstream_id, .. } => {
            Some(workstream_id.as_str() == target.as_str())
        }
        _ => None,
    }
}

/// Build the live aggregation stream for one workstream (no ring-buffer
/// replay — see module docs).
///
/// Why: the single seam [`routes`]' handler drives; kept as a free function
/// (not inlined into the handler) so a unit test can exercise the fan-out
/// logic directly against a `SharedWorkstreamStore`, without going through
/// axum at all. As of Phase 1B (issue #3299) this is a thin adapter over
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
    trusty_agents_common::transport::aggregate_live(
        workstream_id,
        TcodeEventSource,
        WorkstreamMembership(store),
        classify,
    )
}

/// Serialise one [`WorkstreamEventEnvelope`] as an SSE `data:` frame — mirrors
/// `crate::serve::http::sse_event_for`'s (practically infallible) fallback
/// convention.
fn sse_event_for(envelope: &WorkstreamEventEnvelope) -> SseEvent {
    SseEvent::default()
        .json_data(envelope)
        .unwrap_or_else(|_| SseEvent::default().data("{}"))
}

/// Map a [`StoreError`] onto the JSON-RPC error taxonomy (mirrors
/// `crate::workstreams::protocol::map_store_err`, kept separate since that
/// function is private to `protocol`).
fn map_store_err(err: StoreError) -> RpcError {
    match err {
        StoreError::NotFound(id) => RpcError::not_found(format!("workstream not found: {id}")),
        StoreError::Io(_) | StoreError::Serialize(_) => RpcError::internal(err.to_string()),
    }
}

/// Shared axum state for this route: just the workstream store.
#[derive(Clone)]
struct WorkstreamSseState {
    store: SharedWorkstreamStore,
}

/// `GET /workstreams/{id}/events` — SSE aggregation over a workstream's
/// bound sessions.
///
/// Why: see module docs for the full fan-out design.
/// What: `400` (`-32602 invalid_params`) if `id` is not a valid UUID; `404`
/// (`-32002 not_found`) if it names no existing workstream (a CLOSED
/// workstream is NOT a 404 — see module docs); otherwise `200` with an SSE
/// stream from [`aggregate_live`], kept alive with axum's default
/// keep-alive comments (matching `session_events_sse`'s convention).
/// Test: `sse_tests::unknown_id_returns_404`,
/// `sse_tests::malformed_id_returns_400`.
async fn workstream_events_sse(
    State(state): State<WorkstreamSseState>,
    Path(id): Path<String>,
) -> Result<
    Sse<impl Stream<Item = Result<SseEvent, Infallible>>>,
    (axum::http::StatusCode, Json<Response>),
> {
    let ws_id = Uuid::parse_str(&id).map(WorkstreamId::from).map_err(|e| {
        let err = RpcError::invalid_params(format!("invalid workstream id: {e}"));
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(Response::err(None, err.code, err.message)),
        )
    })?;

    {
        let mut store = state.store.lock().await;
        store.get(ws_id).await.map_err(map_store_err).map_err(|e| {
            (
                axum::http::StatusCode::NOT_FOUND,
                Json(Response::err(None, e.code, e.message)),
            )
        })?;
    }

    let stream = aggregate_live(ws_id, state.store.clone()).map(|env| Ok(sse_event_for(&env)));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Build the `GET /workstreams/{id}/events` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// route is unit-testable via `tower::util::ServiceExt::oneshot` on its own,
/// exactly like every `rest::*` group — `crate::serve::rest::workstreams`
/// merges the CRUD/activation REST surface, this merges the observation
/// surface, both sharing the SAME `SharedWorkstreamStore` handle the daemon
/// boot path constructs.
/// What: one route, `with_state`-erased to `axum::Router<()>` so the caller
/// can `.merge()` it.
/// Test: `sse_tests::*`.
pub fn routes(store: SharedWorkstreamStore) -> AxumRouter {
    AxumRouter::new()
        .route("/workstreams/{id}/events", get(workstream_events_sse))
        .with_state(WorkstreamSseState { store })
}

#[cfg(test)]
#[path = "sse_tests.rs"]
mod sse_tests;
