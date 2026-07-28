//! Peer-bus HTTP surface (`/api/v1/bus/*`).
//!
//! Why: DOC-60 §4 hosts the bus in the `tm` daemon, so trusty-agents processes
//! reach it over the daemon's existing loopback HTTP API rather than a second
//! transport. Modeling the endpoints on the existing session routes (SSE for
//! the live stream, JSON for everything else) keeps the idiom uniform — §9 is
//! explicit that this surface should mirror the existing patterns rather than
//! invent a new one.
//! What: five handlers — register/deregister/list an instance, publish a peer
//! message, and subscribe to one instance's inbound stream.
//! Test: `bus::tests` — `route_register_returns_instance_id`,
//! `route_publish_to_dead_instance_is_410`, `route_list_instances`.

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post},
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::daemon::state::DaemonState;

use super::envelope::{BusEnvelope, BusPayload, CallerIdentity};
use super::error::BusError;
use super::registry::{InstanceMeta, PeerTarget};

/// The peer bus's self-contained sub-router, merged into the daemon router.
///
/// Why: owning the route table here (rather than listing the paths in
/// `api.rs`) keeps the bus's HTTP surface beside the registry and envelope it
/// operates on, mirroring `manager_router`. Adding an endpoint later touches
/// one file instead of two.
/// What: registration (list/create/delete), publish, and the per-instance SSE
/// subscription.
/// Test: `route_register_returns_instance_id`, `route_list_instances`,
/// `route_publish_delivers_and_returns_envelope`.
pub fn bus_router() -> Router<Arc<DaemonState>> {
    Router::new()
        .route(
            "/api/v1/bus/instances",
            get(list_instances).post(register_instance),
        )
        .route(
            "/api/v1/bus/instances/{instance_id}",
            delete(deregister_instance),
        )
        .route("/api/v1/bus/publish", post(publish_message))
        .route(
            "/api/v1/bus/subscribe/{instance_id}",
            get(subscribe_instance),
        )
}

/// Request body for `POST /api/v1/bus/instances`.
///
/// Why: an assistant announces itself by definition, not by id — the id is
/// minted by the daemon so it cannot be forged or collided.
/// What: the §6a definition id plus an optional project scope (recorded, not
/// routed on; cross-project addressing is DOC-60 §12 Q4, deferred).
/// Test: `route_register_returns_instance_id`.
#[derive(Debug, Deserialize)]
pub struct RegisterInstanceRequest {
    /// §6a — the persona config this instance runs.
    pub definition_id: String,
    /// Optional project scope, recorded for a future §12 Q4 resolution.
    #[serde(default)]
    pub project: Option<String>,
}

/// Response body for the instance listing.
///
/// Test: `route_list_instances`.
#[derive(Debug, Serialize)]
pub struct ListInstancesResponse {
    /// Every live instance, ordered by registration sequence.
    pub instances: Vec<InstanceMeta>,
}

/// The `to` field of a publish request — DOC-60 §5.3's two addressing modes.
///
/// Why: instance bypass is owner-ratified (DOC-60 §12 Q8, decided in this
/// change's favor), so the wire shape must express both modes and must make
/// which one was intended unambiguous. It is NOT a fallback pair: supplying
/// both fields selects bypass and, if that instance is gone, the request
/// fails — it does not quietly retry against the definition. See
/// [`super::registry`]'s module doc for why.
/// What: either or both ids; `instance_id` wins when both are present.
/// Test: `target_prefers_instance_when_both_supplied`,
/// `route_publish_to_dead_instance_is_410`.
#[derive(Debug, Deserialize)]
pub struct PublishTarget {
    /// §6b — address one specific live instance (bypass mode).
    #[serde(default)]
    pub instance_id: Option<String>,
    /// §6a — address a definition; the registry resolves the instance.
    #[serde(default)]
    pub definition_id: Option<String>,
}

impl PublishTarget {
    /// Resolve the request's addressing mode.
    ///
    /// Why: `instance_id` takes precedence because supplying it is an explicit
    /// statement that thread continuity with THAT instance matters. Silently
    /// preferring the definition would discard the sender's stated intent.
    /// What: [`PeerTarget::Instance`] when `instance_id` is present, else
    /// [`PeerTarget::Definition`], else [`BusError::InvalidTarget`].
    /// Test: `target_prefers_instance_when_both_supplied`,
    /// `target_requires_one_id`.
    pub fn to_peer_target(&self) -> Result<PeerTarget, BusError> {
        match (&self.instance_id, &self.definition_id) {
            (Some(i), _) => Ok(PeerTarget::Instance(i.clone())),
            (None, Some(d)) => Ok(PeerTarget::Definition(d.clone())),
            (None, None) => Err(BusError::InvalidTarget(
                "supply instance_id (bypass) or definition_id (DOC-60 §5.3)".into(),
            )),
        }
    }
}

/// Request body for `POST /api/v1/bus/publish`.
///
/// Why: `message_id` and `ts` are absent by design — the daemon mints both so
/// ids stay unique and the append-only log stays monotonic.
/// What: the §6c caller, the target, the payload, and an optional thread
/// pointer.
/// Test: `route_publish_to_dead_instance_is_410`.
#[derive(Debug, Deserialize)]
pub struct PublishRequest {
    /// §6c — who is sending.
    pub from: CallerIdentity,
    /// Where it goes.
    pub to: PublishTarget,
    /// What it carries.
    pub payload: BusPayload,
    /// The `message_id` this replies to, if any.
    #[serde(default)]
    pub in_reply_to: Option<String>,
}

/// `POST /api/v1/bus/instances` — register a live instance.
///
/// Why: registration is what makes an assistant addressable at all; without
/// it neither addressing mode can resolve anything.
/// What: mints and returns the instance metadata, `201 Created`.
/// Test: `route_register_returns_instance_id`.
pub async fn register_instance(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<RegisterInstanceRequest>,
) -> Result<(StatusCode, Json<InstanceMeta>), BusError> {
    let meta = state
        .bus()
        .registry()
        .register(&req.definition_id, req.project)?;
    Ok((StatusCode::CREATED, Json(meta)))
}

/// `DELETE /api/v1/bus/instances/{instance_id}` — deregister on exit.
///
/// Why: a stale registration would let definition-addressed delivery resolve
/// to a dead instance, turning a clean `404` into a confusing `409`.
/// What: `204 No Content` when removed, `404` when it was not registered.
/// Test: `route_deregister_removes_instance`.
pub async fn deregister_instance(
    State(state): State<Arc<DaemonState>>,
    Path(instance_id): Path<String>,
) -> StatusCode {
    if state.bus().registry().deregister(&instance_id) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

/// `GET /api/v1/bus/instances` — the live roster.
///
/// Why: a sender choosing a peer, and an operator debugging delivery, both
/// need to see what is actually addressable right now.
/// What: every live [`InstanceMeta`], ordered by registration sequence.
/// Test: `route_list_instances`.
pub async fn list_instances(State(state): State<Arc<DaemonState>>) -> Json<ListInstancesResponse> {
    Json(ListInstancesResponse {
        instances: state.bus().registry().live(),
    })
}

/// `POST /api/v1/bus/publish` — send one peer message.
///
/// Why: the §5.3 delivery path. Every failure reaches the sender as a distinct
/// status (`404` no live instance, `410` the named instance is gone, `409`
/// registered but unattached) per §4's fail-closed rule.
/// What: resolves the target, publishes, and returns the stamped envelope with
/// `202 Accepted` so the sender holds the `message_id` for threading.
/// Test: `route_publish_to_dead_instance_is_410`,
/// `route_publish_delivers_and_returns_envelope`.
pub async fn publish_message(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<PublishRequest>,
) -> Result<(StatusCode, Json<BusEnvelope>), BusError> {
    let target = req.to.to_peer_target()?;
    let envelope = state
        .bus()
        .publish(req.from, &target, req.payload, req.in_reply_to)?;
    Ok((StatusCode::ACCEPTED, Json(envelope)))
}

/// `GET /api/v1/bus/subscribe/{instance_id}` — an instance's inbound stream.
///
/// Why: delivery is per-instance, so a subscriber attaches to one instance's
/// own channel rather than filtering a global firehose — the defect DOC-60 §2
/// identifies in the existing `/sessions/{id}/events` path.
/// What: SSE of [`BusEnvelope`] JSON; `410 Gone` when the instance is not
/// registered, so a client cannot sit on a stream that will never produce.
/// Test: `subscribe_to_dead_instance_errors`,
/// `publish_reaches_only_target_instance`.
pub async fn subscribe_instance(
    State(state): State<Arc<DaemonState>>,
    Path(instance_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, BusError> {
    let rx = state.bus().subscribe(&instance_id)?;
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(envelope) => Event::default().json_data(envelope).ok().map(Ok),
        // A lagged subscriber missed envelopes; skip rather than tear the
        // stream down, matching the existing session SSE handlers.
        Err(_) => None,
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
