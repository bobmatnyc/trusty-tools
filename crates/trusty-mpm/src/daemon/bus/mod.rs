//! Peer message bus — daemon-side foundation for DOC-60 §5.3.
//!
//! Why: PR #4240 closed the one working assistant-to-assistant interaction the
//! codebase had (the Izzie ↔ cto-assistant peer-consult lane) as the direct
//! consequence of ADR-0024's kind-based delegation gate. That was correct —
//! assistants are virtual twins that may communicate but never delegate — but
//! it left trusty-agents personas with *no* agent-to-agent path of any kind.
//! DOC-60 §5.3 is the specified replacement; this module is its daemon side.
//! What: [`PeerBus`], holding the §6b instance registry, the per-instance
//! delivery channels, and the DOC-60 §9 durable JSONL stream (written through
//! the EXISTING [`AuditLogger`](crate::daemon::audit::AuditLogger), not a
//! second writer). Submodules: [`envelope`] is the §11 schema, [`registry`]
//! resolves both addressing modes, [`error`] is the §4 fail-closed contract,
//! and [`routes`] is the HTTP surface.
//! Test: `tests` — the module's suite covers publish, both addressing modes,
//! the bypass failure mode, the durable record, and the #4271 lag contract.
//!
//! ## Scope: step 1 of DOC-60 §5.3, and what it deliberately excludes
//!
//! - **Targets a RUNNING instance only.** DOC-60 §7's durable inbox and
//!   queue-not-spawn behavior are NOT built here; a message to a definition
//!   with nothing running fails closed per §4 rather than queueing.
//! - **Cross-project addressing** (§12 Q4), **retention policy** (§12 Q1), and
//!   **version-skew negotiation** (§12 Q2/Q5) are all deferred to the owner.
//! - **Streaming token deltas stay off this bus.** High-volume telemetry
//!   (`Event::AgentMessageDelta` and equivalents) remains on the existing
//!   per-crate buses, which DOC-60 §3 keeps alive for exactly that job. This
//!   bus carries addressed messages, not a firehose — the per-instance channel
//!   capacity is sized accordingly.
//! - **The client and the peer-message tool are separate changes.** This is the
//!   daemon-side foundation only.
//!
//! [`PeerBus`]: crate::daemon::bus::PeerBus
//! [`envelope`]: crate::daemon::bus::envelope
//! [`registry`]: crate::daemon::bus::registry
//! [`error`]: crate::daemon::bus::error
//! [`routes`]: crate::daemon::bus::routes

use std::path::Path;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::daemon::audit::{AuditLogger, BUS_STREAM};

pub mod envelope;
pub mod error;
pub mod registry;
pub mod routes;

#[cfg(test)]
mod tests;

pub use envelope::{
    BusEdge, BusEnvelope, BusPayload, CallerIdentity, CallerKind, ChannelOrigin, DeliveryState,
    Recipient,
};
pub use error::BusError;
pub use registry::{
    DeliveryOutcome, INSTANCE_CHANNEL_CAPACITY, InstanceMeta, InstanceRegistry, LiveInstance,
    PeerTarget,
};

/// The daemon's peer message bus.
///
/// Why: DOC-60 §4 makes `tm` the bus host, so the registry, the delivery
/// channels, and the durable log must live behind one handle the daemon state
/// can hold and the HTTP routes can borrow. Bundling them also keeps the
/// publish path atomic in the way that matters: an envelope is stamped,
/// logged, and delivered (or logged as dropped) without an interleaving caller
/// seeing a half-applied state.
/// What: an [`InstanceRegistry`] and an [`AuditLogger`] pointed at the
/// `logs_dir/bus/` stream.
/// Test: `tests`.
#[derive(Debug)]
pub struct PeerBus {
    /// §6b instance registry — the resolution table for both addressing modes.
    registry: InstanceRegistry,
    /// DOC-60 §9 durable stream, reusing the daemon's existing JSONL writer.
    audit: AuditLogger,
}

impl PeerBus {
    /// Build a bus writing its durable stream under `logs_dir/bus/`.
    ///
    /// Why: §9 specifies a `bus/` stream "alongside the existing `overseer/`
    /// one", fed by the same logger. Constructing it here means no call site
    /// has to know the stream name.
    /// What: an empty registry plus an [`AuditLogger`] for [`BUS_STREAM`]. No
    /// IO happens until the first publish.
    /// Test: `publish_writes_durable_jsonl_record`.
    pub fn new(logs_dir: &Path) -> Self {
        Self {
            registry: InstanceRegistry::default(),
            audit: AuditLogger::for_stream(logs_dir, BUS_STREAM),
        }
    }

    /// The §6b instance registry.
    ///
    /// Test: `list_returns_live_instances`.
    pub fn registry(&self) -> &InstanceRegistry {
        &self.registry
    }

    /// The durable stream's resolved path.
    ///
    /// Test: `publish_writes_durable_jsonl_record`.
    pub fn log_path(&self) -> &Path {
        self.audit.path()
    }

    /// Subscribe to one instance's inbound messages.
    ///
    /// Why: delivery is per-instance, not broadcast-then-filter — see
    /// [`registry::LiveInstance`] for why that distinction is the point of
    /// this bus. A subscriber therefore attaches to a specific instance's
    /// channel, and attaching to a dead one is an error rather than a silently
    /// empty stream.
    /// What: resolves `instance_id` and returns a receiver on its channel.
    /// Test: `publish_reaches_only_target_instance`,
    /// `subscribe_to_dead_instance_errors`.
    pub fn subscribe(
        &self,
        instance_id: &str,
    ) -> Result<broadcast::Receiver<BusEnvelope>, BusError> {
        Ok(self.registry.resolve_instance(instance_id)?.tx.subscribe())
    }

    /// Verify a claimed sender identity against the live registry.
    ///
    /// Why: `from` arrives entirely client-asserted over loopback HTTP. Left
    /// unverified, assistant A could publish to assistant B claiming
    /// `kind: "user"`, and B — which by §5.3's design cannot distinguish a
    /// lateral message from a user message by shape — would read it as a user
    /// instruction and act on it. That is assistant-to-assistant DELEGATION
    /// reconstituted through the very bus ADR-0024 closed it from, so the
    /// sender's claim has to be checked against something the sender does not
    /// control. The registry is that something.
    /// What: resolves the claimed `instance_id` through the registry's
    /// EXISTING [`InstanceRegistry::resolve_instance`] (no second lookup path
    /// — this repo's common-entry-point rule), then re-stamps
    /// `definition_id` from the registration rather than trusting the
    /// caller's copy, so the §9 log records the definition the daemon knows
    /// rather than the one the sender asserted. The registry error is
    /// deliberately MAPPED, not propagated: a miss here means the SENDER is
    /// unverified ([`BusError::UnregisteredSender`], 403), which is a
    /// different fault with a different recovery than the recipient being
    /// gone ([`BusError::InstanceGone`], 410).
    /// Test: `publish_rejects_unregistered_sender`,
    /// `publish_restamps_sender_definition_from_registry`.
    fn verified_sender(&self, mut from: CallerIdentity) -> Result<CallerIdentity, BusError> {
        let claimed = from.instance_id.clone().ok_or_else(|| {
            BusError::InvalidCaller("peer sender requires instance_id (DOC-60 §6c)".into())
        })?;
        let live =
            self.registry
                .resolve_instance(&claimed)
                .map_err(|_| BusError::UnregisteredSender {
                    instance_id: claimed,
                })?;
        from.definition_id = Some(live.meta.definition_id);
        Ok(from)
    }

    /// Publish one peer message, fail-closed.
    ///
    /// Why: this is the §5.3 delivery path and the §4 fail-closed contract in
    /// one place.
    /// What:
    /// 1. structurally validates the caller identity (§6c);
    /// 2. DERIVES the §5 edge from the caller's kind via
    ///    [`CallerKind::peer_edge`], which also gates the path to
    ///    `assistant_instance` senders — a `user`-kind publish is rejected
    ///    here, not silently recorded as lateral traffic;
    /// 3. VERIFIES the claimed sender against the live registry
    ///    ([`verified_sender`](Self::verified_sender));
    /// 4. resolves `target` through the registry — with NO fallback between
    ///    addressing modes (see [`registry`]'s module doc);
    /// 5. on resolution failure, stamps a [`DeliveryState::Dropped`] envelope,
    ///    logs it, and returns the error to the sender;
    /// 6. on success, stamps a [`DeliveryState::Delivered`] envelope, sends it
    ///    to that instance's channel, and logs what actually happened. A
    ///    registered instance with no attached subscriber yields
    ///    [`BusError::NoSubscriber`] and a `Dropped` record rather than a
    ///    silent drop — DOC-60 §7's durable inbox is what will make that case
    ///    queue instead, and it is deferred.
    ///
    /// **The delivery guarantee, and the backpressure that buys it (#4271).**
    /// An envelope this method records as `Delivered` stays readable by every
    /// attached subscriber until that subscriber reads it. No later publish
    /// through this method evicts it. Step 6 delegates to
    /// [`LiveInstance::deliver`], which holds the instance's delivery gate
    /// across both the measurement and the send: when the recipient's channel
    /// already holds [`INSTANCE_CHANNEL_CAPACITY`] envelopes its slowest
    /// subscriber has not read, the next send could only be taken by
    /// overwriting the oldest of them, so the NEW envelope is refused with
    /// [`BusError::SubscriberLagged`] (503) and recorded `Dropped`. The gate is
    /// what makes the guarantee hold under concurrent publishers rather than
    /// only under one — see `deliver` for why an unguarded check is stale by
    /// the time the send runs.
    ///
    /// Before #4271 the send was made anyway. `broadcast::Sender::send` answers
    /// `Ok` for a lagging receiver, so the log recorded `Delivered` for the
    /// envelope that displaced an unread one, the sender got `202 Accepted`,
    /// and the recipient was never told — a `delivered` record for a message
    /// that reached no one. Refusing the newest message rather than silently
    /// discarding the oldest keeps the §9 log answerable, which is the whole
    /// reason DOC-60 §4 forbids a silent drop. A healthy subscriber never
    /// reaches saturation and is unaffected.
    ///
    /// **What the refusal costs, and why nothing here bounds it.** Saturation
    /// is measured across every attached receiver, and `broadcast` cannot send
    /// to a subset, so ONE subscriber that stops draining makes this instance
    /// refuse every publish to it — including on behalf of healthy
    /// co-subscribers. That wedge clears when the stalled receiver drops
    /// (a client that exits closes its SSE connection, and `Receiver::drop`
    /// drains the backlog it never read) or when an operator deregisters the
    /// instance. Nothing in this daemon puts a timer on it, deliberately: the
    /// only sender-side lever `broadcast` offers is dropping the channel, which
    /// would discard envelopes this log has already recorded `Delivered` and so
    /// re-create #4271 by another route. A per-subscriber buffer that could
    /// evict one client without touching the others is DOC-60 §7's durable
    /// inbox, deferred to the owner. Until then the failure is loud — a `warn!`
    /// and a 503 naming the instance on every refused publish — rather than
    /// bounded.
    ///
    /// **What the durable §9 log does and does not contain.** Once a sender is
    /// verified (step 3), EVERY outcome is recorded — delivered or dropped —
    /// because a log holding only successes cannot answer the question
    /// ADR-0019 exists to answer ("was it sent, or just never read?"). Steps
    /// 1–3 record NOTHING: a request that fails structural validation, the
    /// kind gate, or sender verification never became an envelope. Writing one
    /// would mean stamping an unverified — possibly forged — identity into the
    /// log in the same shape as attributable traffic, which would corrupt the
    /// §9 record it is meant to protect. Rejected requests are surfaced to the
    /// caller as 4xx, which is where an unauthenticated attempt belongs.
    ///
    /// Returns the stamped envelope so the sender holds the `message_id` it
    /// needs for `in_reply_to` threading and §10 provenance.
    /// Test: `publish_delivers_to_definition_addressed_instance`,
    /// `bypass_publish_stamps_both_ids`, `failed_publish_logs_dropped_envelope`,
    /// `publish_without_subscriber_errors`, `publish_rejects_forged_user_kind`,
    /// `rejected_publish_writes_no_durable_record`,
    /// `delivered_records_match_what_a_stalled_subscriber_receives`,
    /// `saturated_publish_is_dropped_in_the_durable_log`,
    /// `concurrent_publishers_never_evict_a_delivered_envelope`.
    pub fn publish(
        &self,
        from: CallerIdentity,
        target: &PeerTarget,
        payload: BusPayload,
        in_reply_to: Option<String>,
    ) -> Result<BusEnvelope, BusError> {
        from.validate()?;
        let edge = from.kind.peer_edge()?;
        let from = self.verified_sender(from)?;

        let live = match self.registry.resolve(target) {
            Ok(live) => live,
            Err(e) => {
                // A dropped bypass record cannot name a definition: the
                // instance was already gone, so there was nothing to resolve
                // one from. See `Recipient`'s doc comment.
                let to = match target {
                    PeerTarget::Definition(d) => Recipient {
                        instance_id: None,
                        definition_id: Some(d.clone()),
                    },
                    PeerTarget::Instance(i) => Recipient {
                        instance_id: Some(i.clone()),
                        definition_id: None,
                    },
                };
                self.audit.log_record(&BusEnvelope::new(
                    edge,
                    from,
                    to,
                    payload,
                    in_reply_to,
                    DeliveryState::Dropped,
                ));
                return Err(e);
            }
        };

        let mut envelope = BusEnvelope::new(
            edge,
            from,
            Recipient {
                instance_id: Some(live.meta.instance_id.clone()),
                definition_id: Some(live.meta.definition_id.clone()),
            },
            payload,
            in_reply_to,
            DeliveryState::Delivered,
        );

        // The durable record is written AFTER the delivery attempt so it states
        // what actually happened. Logging `Delivered` before knowing whether
        // the channel could take the envelope would put a lie in the one log
        // that is supposed to settle "sent or not" — #4271 is what that lie
        // looked like in practice.
        match live.deliver(envelope.clone()) {
            DeliveryOutcome::Delivered => {
                self.audit.log_record(&envelope);
                Ok(envelope)
            }
            DeliveryOutcome::Saturated => {
                envelope.delivery_state = DeliveryState::Dropped;
                self.audit.log_record(&envelope);
                tracing::warn!(
                    instance_id = %live.meta.instance_id,
                    capacity = INSTANCE_CHANNEL_CAPACITY,
                    message_id = %envelope.message_id,
                    "peer bus refused a publish: the recipient's channel is full \
                     of unread envelopes (#4271)"
                );
                Err(BusError::SubscriberLagged {
                    instance_id: live.meta.instance_id,
                })
            }
            DeliveryOutcome::NoSubscriber => {
                envelope.delivery_state = DeliveryState::Dropped;
                self.audit.log_record(&envelope);
                Err(BusError::NoSubscriber {
                    instance_id: live.meta.instance_id,
                })
            }
        }
    }
}

/// Shared handle the daemon state holds.
pub type SharedBus = Arc<PeerBus>;
