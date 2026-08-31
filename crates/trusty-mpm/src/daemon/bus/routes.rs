//! Peer-bus HTTP surface (`/api/v1/bus/*`).
//!
//! Why: DOC-60 §4 hosts the bus in the `tm` daemon, so trusty-agents processes
//! reach it over the daemon's existing loopback HTTP API rather than a second
//! transport. Modeling the endpoints on the existing session routes (SSE for
//! the live stream, JSON for everything else) keeps the idiom uniform — §9 is
//! explicit that this surface should mirror the existing patterns rather than
//! invent a new one.
//! What: five handlers — register/deregister/list an instance, publish a peer
//! message, and subscribe to one instance's inbound stream. Since #6288 slice
//! 5 the four request/response verbs are ALSO JSON-RPC methods on the daemon's
//! Unix socket: each handler is now a delegator over a transport-neutral
//! `*_op` body ([`register_op`], [`deregister_op`], [`list_op`], [`publish_op`])
//! that both transports call, so one route keeps one implementation.
//! [`subscribe_instance`] is deliberately NOT among them — it is SSE, and it
//! needs the `RpcStreamMethod` seam slice 6 owns. No RPC method is registered
//! for it, and its handler is untouched here.
//! Test: `bus::tests` — `route_register_returns_instance_id`,
//! `route_publish_to_dead_instance_is_410`, `route_list_instances`; the
//! transport-parity cases live in `daemon::rpc::registry::tests`.

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

use crate::daemon::error::DaemonError;
use crate::daemon::state::DaemonState;

use super::envelope::{BusEnvelope, BusPayload, CallerIdentity};
use super::error::BusError;
use super::inbox::InboxItem;
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
    // #6288: the body is shared with `mpm.bus.register` over the Unix socket.
    Ok((StatusCode::CREATED, Json(register_op(&state, req)?)))
}

/// [`register_instance`]'s body, with no transport in it (#6288 slice 5).
///
/// Test: `parity_bus_register_agrees_across_transports`.
pub fn register_op(
    state: &Arc<DaemonState>,
    req: RegisterInstanceRequest,
) -> Result<InstanceMeta, BusError> {
    state
        .bus()
        .registry()
        .register(&req.definition_id, req.project)
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
    // #6288: the body is shared with `mpm.bus.deregister` over the Unix socket.
    // The status pair is preserved exactly — this route has never had a body.
    match deregister_op(&state, &instance_id) {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

/// The acknowledgement `mpm.bus.deregister` answers with.
///
/// Why: the HTTP route says "removed" with a bodyless `204`, which a JSON-RPC
/// result cannot express — a result is a JSON value or it is an error. Rather
/// than inventing a null result, the socket answers a one-field record, and
/// the miss stays an ERROR on both transports (`404` / `CODE_NOT_FOUND`) so a
/// caller still cannot mistake "there was nothing to remove" for a removal.
/// Test: `parity_bus_deregister_agrees_across_transports`,
/// `rpc_bus_deregister_unknown_instance_reports_not_found`.
#[derive(Debug, Serialize)]
pub struct DeregisterAck {
    /// Always `true` — a failure is an error frame, never `false` here.
    pub deregistered: bool,
}

/// [`deregister_instance`]'s body, with no transport in it (#6288 slice 5).
///
/// Why the error is a [`DaemonError`] rather than a [`BusError`]: `BusError` has
/// no "was not registered" variant, and the two candidates both say the wrong
/// thing — `InstanceGone` is `410` (this route has always answered `404`) and
/// `NoLiveInstance` is about a DEFINITION that resolved to nothing. Inventing a
/// bus variant to serve one route would put a fifth addressing failure into an
/// enum DOC-60 §4 keeps deliberately narrow.
///
/// # Errors
///
/// [`DaemonError::NotFound`] when `instance_id` was not a live registration.
///
/// Test: `parity_bus_deregister_agrees_across_transports`.
pub fn deregister_op(
    state: &Arc<DaemonState>,
    instance_id: &str,
) -> Result<DeregisterAck, DaemonError> {
    if state.bus().registry().deregister(instance_id) {
        Ok(DeregisterAck { deregistered: true })
    } else {
        Err(DaemonError::NotFound(format!(
            "bus instance '{instance_id}' is not registered"
        )))
    }
}

/// `GET /api/v1/bus/instances` — the live roster.
///
/// Why: a sender choosing a peer, and an operator debugging delivery, both
/// need to see what is actually addressable right now.
/// What: every live [`InstanceMeta`], ordered by registration sequence.
/// Test: `route_list_instances`.
pub async fn list_instances(State(state): State<Arc<DaemonState>>) -> Json<ListInstancesResponse> {
    // #6288: the body is shared with `mpm.bus.list` over the Unix socket.
    Json(list_op(&state))
}

/// [`list_instances`]'s body, with no transport in it (#6288 slice 5).
///
/// Test: `parity_bus_list_agrees_across_transports`.
pub fn list_op(state: &Arc<DaemonState>) -> ListInstancesResponse {
    ListInstancesResponse {
        instances: state.bus().registry().live(),
    }
}

/// `POST /api/v1/bus/publish` — send one peer message.
///
/// Why: the §5.3 delivery path. Every failure reaches the sender as a distinct
/// status (`400` malformed request or a caller kind this path does not carry,
/// `403` the claimed sender is not a live registration, `404` no live
/// instance, `410` the named instance is gone, `409` registered but
/// unattached) per §4's fail-closed rule.
///
/// **A slow recipient no longer refuses the publish (#6462).** Between #4271
/// and #6462 this endpoint answered `503` when the recipient's shared channel
/// was full of unread envelopes, and one stalled subscriber made every publish
/// to that instance answer `503` — healthy co-subscribers included, until it
/// drained, dropped, or the instance was deregistered. Each subscription now
/// has its own buffer, so a stalled client falls behind alone and this endpoint
/// answers `202` while that client's inbox displaces its own oldest unread
/// envelope. The loss is recorded in the DOC-60 §9 durable log and announced to
/// the affected client as a [`LAGGED_EVENT`] frame; the sender is not involved
/// and has nothing to retry. `503` is no longer a status this endpoint returns.
/// What: resolves the target, publishes, and returns the stamped envelope with
/// `202 Accepted` so the sender holds the `message_id` for threading. The
/// `from` field is a CLAIM: [`PeerBus::publish`](crate::daemon::bus::PeerBus::publish)
/// verifies it against the registry and re-stamps the definition, so a caller
/// cannot choose the identity its message is recorded under.
/// Test: `route_publish_to_dead_instance_is_410`,
/// `route_publish_delivers_and_returns_envelope`,
/// `route_publish_forged_user_kind_is_400`,
/// `route_publish_unregistered_sender_is_403`,
/// `route_publish_past_a_stalled_subscriber_is_accepted`.
pub async fn publish_message(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<PublishRequest>,
) -> Result<(StatusCode, Json<BusEnvelope>), BusError> {
    // #6288: the body is shared with `mpm.bus.publish` over the Unix socket.
    Ok((StatusCode::ACCEPTED, Json(publish_op(&state, req)?)))
}

/// [`publish_message`]'s body, with no transport in it (#6288 slice 5).
///
/// Why this is a call-signature move and nothing more: DOC-60 §4's fail-closed
/// sequencing lives inside
/// [`PeerBus::publish`](crate::daemon::bus::PeerBus::publish) — structural
/// validation of the caller identity, then the `assistant_instance` edge check,
/// then sender verification against the registry, then target resolution, then
/// the delivery attempt, with the durable record written to state what actually
/// happened. None of that moved, and none of it is re-derived here, so the
/// socket path rejects at exactly the same four points the HTTP path does.
/// Every rejection reaches the caller as a [`BusError`], which both transports
/// render from the same `status` table.
///
/// # Errors
///
/// Every [`BusError`] `PeerBus::publish` can raise, plus
/// [`BusError::InvalidTarget`] when the request names neither addressing mode.
/// A recipient whose subscriber has stopped reading is NOT among them since
/// #6462 — see [`publish_message`]'s doc.
///
/// Test: `parity_bus_publish_agrees_across_transports`,
/// `rpc_bus_publish_rejects_a_forged_user_kind`,
/// `rpc_bus_publish_rejects_an_unregistered_sender`,
/// `rpc_bus_publish_to_a_dead_instance_is_gone`,
/// `rpc_bus_publish_without_a_subscriber_is_conflict`,
/// `rpc_bus_publish_without_a_target_is_invalid`,
/// `route_publish_past_a_stalled_subscriber_is_accepted`.
pub fn publish_op(state: &Arc<DaemonState>, req: PublishRequest) -> Result<BusEnvelope, BusError> {
    let target = req.to.to_peer_target()?;
    state
        .bus()
        .publish(req.from, &target, req.payload, req.in_reply_to)
}

/// The SSE event name a lag notice arrives under (#4271).
///
/// Why its own name rather than another default `message` frame: a client
/// deserializing every frame as a [`BusEnvelope`] must not have to guess. A
/// named event is ignored by a client that does not know it, and dispatched by
/// one that does.
/// Test: `subscribe_stream_reports_lag_instead_of_swallowing_it`.
pub const LAGGED_EVENT: &str = "lagged";

/// What a `lagged` SSE frame carries.
///
/// Why: a subscriber that fell behind needs four things to recover — that it
/// happened, how many envelopes it missed, where to read them, and under which
/// key. The durable §9 JSONL stream holds every envelope the bus recorded as
/// delivered plus, since #6462, one
/// [`InboxMiss`](crate::daemon::bus::InboxMiss) per envelope this subscription
/// lost, keyed by `subscription_id` — so naming the path and the id makes the
/// recovery a file read rather than a lost message.
/// What: the instance, the subscription, the count its inbox displaced, and the
/// durable log's resolved path.
/// Test: `subscribe_stream_reports_lag_instead_of_swallowing_it`.
#[derive(Debug, Serialize)]
pub struct LagNotice {
    /// The instance whose subscription fell behind.
    pub instance_id: String,
    /// Which subscription fell behind — the key its eviction records carry.
    pub subscription_id: u64,
    /// How many envelopes this subscription's inbox displaced unread.
    pub missed: u64,
    /// Path to the DOC-60 §9 durable stream to re-read them from.
    pub durable_log: String,
}

/// `GET /api/v1/bus/subscribe/{instance_id}` — an instance's inbound stream.
///
/// Why: delivery is per-instance, so a subscriber attaches to one instance's
/// own channel rather than filtering a global firehose — the defect DOC-60 §2
/// identifies in the existing `/sessions/{id}/events` path.
/// What: SSE of [`BusEnvelope`] JSON; `410 Gone` when the instance is not
/// registered, so a client cannot sit on a stream that will never produce.
///
/// **Lag is reported, never swallowed (#4271).** This handler used to map
/// `Lagged(n)` to `None`, borrowing the session SSE handlers' idiom — but those
/// streams carry load-sheddable telemetry, which DOC-60 §3 keeps OFF this bus.
/// Here the skipped frames were addressed messages the durable log had already
/// recorded as delivered, so the recipient lost them and was told nothing. A
/// lag now arrives as a [`LAGGED_EVENT`] frame carrying a [`LagNotice`], plus a
/// `warn!`. Since #6462 this stream reads THIS subscription's own inbox, so a
/// lag here means this client fell
/// [`CLIENT_INBOX_CAPACITY`](super::inbox::CLIENT_INBOX_CAPACITY) behind —
/// nobody else's backlog can produce one, and no other client is slowed by it.
///
/// **The subscription ends when its body is dropped or the instance is
/// deregistered.** Dropping the body detaches the inbox, which is what keeps a
/// disconnected client from holding envelopes nobody will read; deregistration
/// lets the reader drain what it already holds and then ends the stream.
/// Test: `subscribe_to_dead_instance_errors`,
/// `publish_reaches_only_target_instance`,
/// `subscribe_stream_reports_lag_instead_of_swallowing_it`,
/// `deregister_ends_a_subscription_after_it_drains`.
pub async fn subscribe_instance(
    State(state): State<Arc<DaemonState>>,
    Path(instance_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, BusError> {
    let subscription = state.bus().subscribe(&instance_id)?;
    let durable_log = state.bus().log_path().display().to_string();
    let stream = futures::stream::unfold(subscription, move |subscription| {
        let instance_id = instance_id.clone();
        let durable_log = durable_log.clone();
        async move {
            loop {
                match subscription.recv().await? {
                    InboxItem::Envelope(envelope) => match Event::default().json_data(&*envelope) {
                        Ok(event) => return Some((Ok(event), subscription)),
                        Err(e) => {
                            // The envelope is already consumed, so this arm
                            // advances the reader rather than stalling it.
                            tracing::error!(
                                message_id = %envelope.message_id,
                                error = %e,
                                "peer bus envelope could not be serialized onto the SSE stream"
                            );
                        }
                    },
                    InboxItem::Lagged(missed) => {
                        let subscription_id = subscription.subscription_id();
                        tracing::warn!(
                            instance_id = %instance_id,
                            subscription_id,
                            missed,
                            "peer bus subscriber lagged; its inbox displaced unread \
                             envelopes (#6462)"
                        );
                        let frame = lag_frame(&instance_id, subscription_id, missed, &durable_log);
                        return Some((Ok(frame), subscription));
                    }
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Build the [`LAGGED_EVENT`] frame, keeping the count reachable either way.
///
/// Why the fallback: serializing four owned scalars cannot fail, but mapping a
/// hypothetical failure to `None` would reinstate the silent skip this whole
/// change removes. The plain-text form still carries the number that matters.
/// Test: `subscribe_stream_reports_lag_instead_of_swallowing_it`.
fn lag_frame(instance_id: &str, subscription_id: u64, missed: u64, durable_log: &str) -> Event {
    let notice = LagNotice {
        instance_id: instance_id.to_string(),
        subscription_id,
        missed,
        durable_log: durable_log.to_string(),
    };
    match Event::default().event(LAGGED_EVENT).json_data(&notice) {
        Ok(event) => event,
        Err(_) => Event::default()
            .event(LAGGED_EVENT)
            .data(missed.to_string()),
    }
}
