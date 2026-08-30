//! Peer message bus — daemon-side foundation for DOC-60 §5.3.
//!
//! Why: PR #4240 closed the one working assistant-to-assistant interaction the
//! codebase had (the Izzie ↔ cto-assistant peer-consult lane) as the direct
//! consequence of ADR-0024's kind-based delegation gate. That was correct —
//! assistants are virtual twins that may communicate but never delegate — but
//! it left trusty-agents personas with *no* agent-to-agent path of any kind.
//! DOC-60 §5.3 is the specified replacement; this module is its daemon side.
//! What: [`PeerBus`], holding the §6b instance registry, the per-client
//! durable inboxes, and the DOC-60 §9 durable JSONL stream (written through
//! the EXISTING [`AuditLogger`](crate::daemon::audit::AuditLogger), not a
//! second writer). Submodules: [`envelope`] is the §11 schema, [`registry`]
//! resolves both addressing modes, [`inbox`] is §7's per-client delivery
//! boundary, [`error`] is the §4 fail-closed contract, and [`routes`] is the
//! HTTP surface.
//! Test: `tests` — the module's suite covers publish, both addressing modes,
//! the bypass failure mode, the durable record, and the #4271/#6462 delivery
//! contract.
//!
//! ## Scope: step 1 of DOC-60 §5.3, and what it deliberately excludes
//!
//! - **Targets a RUNNING instance only.** #6462 built DOC-60 §7's durable
//!   inbox for a live client's SUBSCRIPTION. §7's other half —
//!   queue-not-spawn for a definition with nothing running — is NOT built
//!   here; such a message fails closed per §4 rather than queueing.
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
//! [`inbox`]: crate::daemon::bus::inbox
//! [`error`]: crate::daemon::bus::error
//! [`routes`]: crate::daemon::bus::routes

use std::path::Path;
use std::sync::Arc;

use crate::daemon::audit::{AuditLogger, BUS_STREAM};

pub mod envelope;
pub mod error;
pub mod inbox;
pub mod registry;
pub mod routes;

#[cfg(test)]
mod tests;

pub use envelope::{
    BusEdge, BusEnvelope, BusPayload, CallerIdentity, CallerKind, ChannelOrigin, DeliveryState,
    Recipient,
};
pub use error::BusError;
pub use inbox::{
    CLIENT_INBOX_CAPACITY, ClientInbox, DeliveryOutcome, INBOX_MISS_RECORD, InboxItem, InboxMiss,
    InboxSet, InboxSubscription, MissReason, MissedEnvelope,
};
pub use registry::{InstanceMeta, InstanceRegistry, LiveInstance, PeerTarget};

/// The daemon's peer message bus.
///
/// Why: DOC-60 §4 makes `tm` the bus host, so the registry, the per-client
/// inboxes, and the durable log must live behind one handle the daemon state
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
    /// this bus. A subscriber therefore attaches to a specific instance, and
    /// attaching to a dead one is an error rather than a silently empty stream.
    /// Since #6462 each subscription gets its OWN bounded buffer rather than a
    /// share of one ring, so how far this subscriber falls behind is its own
    /// business — see [`inbox`]'s module doc.
    /// What: resolves `instance_id` and attaches an [`InboxSubscription`],
    /// which detaches when it is dropped.
    /// Test: `publish_reaches_only_target_instance`,
    /// `subscribe_to_dead_instance_errors`,
    /// `a_detached_subscription_stops_costing_the_instance`.
    pub fn subscribe(&self, instance_id: &str) -> Result<InboxSubscription, BusError> {
        Ok(self.registry.resolve_instance(instance_id)?.subscribe())
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

    /// Write one §9 record per client that lost this envelope.
    ///
    /// Why it returns a bool rather than swallowing (#6462 review): the
    /// "nothing is lost without a record" guarantee is only as strong as these
    /// writes landing, and `log_record` is designed to swallow the failure that
    /// would break it. Reporting the failure up lets the envelope's own record
    /// carry the caveat instead of quietly overstating what the stream holds.
    /// What: writes each [`InboxMiss`] with
    /// [`AuditLogger::try_log_record`](crate::daemon::audit::AuditLogger::try_log_record)
    /// and emits one `warn!` per loss naming the client, returning `true` when
    /// ANY write failed. A failure also gets its own `error!`, because a
    /// durable-log write failing is an operator-visible condition and not just
    /// a caveat on one line.
    /// Test: `eviction_is_recorded_per_client_in_the_durable_log`,
    /// `an_unwritable_log_never_reports_a_clean_delivery`.
    fn record_losses(&self, missed: &[MissedEnvelope]) -> bool {
        let mut unrecorded = false;
        for loss in missed {
            if let Err(e) = self.audit.try_log_record(&loss.record()) {
                unrecorded = true;
                tracing::error!(
                    instance_id = %loss.instance_id,
                    subscription_id = loss.subscription_id,
                    message_id = %loss.envelope.message_id,
                    error = %e,
                    "peer bus could not record an inbox miss; the delivery record \
                     will be marked losses_unrecorded (#6462)"
                );
            }
            tracing::warn!(
                instance_id = %loss.instance_id,
                subscription_id = loss.subscription_id,
                capacity = CLIENT_INBOX_CAPACITY,
                message_id = %loss.envelope.message_id,
                reason = ?loss.reason,
                missed_total = loss.missed_total,
                "peer bus inbox lost an envelope for one client: it must re-read \
                 the durable log (#6462)"
            );
        }
        unrecorded
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
    /// 6. on success, stamps a [`DeliveryState::Delivered`] envelope, fans it
    ///    out into every attached client's inbox, and logs what actually
    ///    happened. A registered instance with no attached subscriber yields
    ///    [`BusError::NoSubscriber`] and a `Dropped` record rather than a
    ///    silent drop — §7's OTHER half, queueing to a definition with nothing
    ///    running, is what will make that case queue instead, and it is
    ///    deferred.
    ///
    /// **The delivery guarantee (#4271, re-cut per client by #6462).** An
    /// envelope this method records as `Delivered` is readable by every
    /// attached subscription until that subscription reads it, UNLESS this
    /// method also wrote an [`InboxMiss`] naming that envelope and the one
    /// subscription that lost it — or marked the delivery record
    /// `losses_unrecorded` because it could not. Nothing is lost without a
    /// record, and no record is silently absent. Step 6 delegates to
    /// [`LiveInstance::deliver`], which fans the envelope into one bounded
    /// buffer per subscription under a single per-instance lock, so concurrent
    /// publishers cannot hand two clients the same two envelopes in opposite
    /// orders. A fan-out that NO inbox accepted — every attached subscription
    /// closed by a deregistration that raced this publish — is
    /// [`DeliveryOutcome::NoSubscriber`], recorded `Dropped` and answered
    /// `409`, never `Delivered`.
    ///
    /// Before #4271 an eviction was invisible: `broadcast::Sender::send`
    /// answers `Ok` for a lagging receiver, so the log recorded `Delivered` for
    /// the envelope that displaced an unread one, the sender got
    /// `202 Accepted`, and the recipient was never told. #4271 closed that by
    /// refusing the publish instead. The eviction is back, and the LIE is what
    /// stays closed: an eviction now costs one `warn!` and one durable record
    /// per lost envelope, plus an [`InboxItem::Lagged`] to the affected reader
    /// at the point of loss.
    ///
    /// **Why publish no longer refuses (#6462).** The refusal was measured
    /// across every attached receiver of one shared ring, so ONE subscriber
    /// that stopped draining made this instance refuse every publish to it,
    /// healthy co-subscribers included, unboundedly. Per-client inboxes remove
    /// the shared quantity that made that possible: a stalled client falls
    /// behind alone, and no publisher or co-subscriber pays for it at all. The
    /// recovery for a client that fell behind is unchanged from #4271: re-read
    /// the §9 durable log, whose path the `lagged` SSE frame carries.
    ///
    /// **What a wedged client still costs, stated exactly.** A client that
    /// disconnects costs nothing — its subscription drops and detaches. A
    /// client that stays attached and never reads — a TCP-wedged SSE body,
    /// where hyper stops polling the response body without dropping the future,
    /// so `Drop` never runs — costs its instance a bounded
    /// [`CLIENT_INBOX_CAPACITY`]-envelope buffer PLUS one [`InboxMiss`] line in
    /// the durable log and one `warn!` per subsequent publish, for as long as
    /// it stays attached. No timer bounds that, deliberately; the operator's
    /// lever is deregistering the instance, and the `warn!` names which
    /// instance and subscription. See [`inbox`]'s module doc.
    ///
    /// **Loss records are written BEFORE the delivery record, and their
    /// failures are observed.** The loss has already happened in memory by the
    /// time either write runs, so the ordering only decides which way an
    /// interruption lies. This way it under-claims — the loss is on record and
    /// the delivery that caused it is not — where the other way round would
    /// leave a `delivered` record with nothing explaining the gap, which is
    /// #4271 exactly.
    ///
    /// Ordering alone is not enough, because
    /// [`AuditLogger::log_record`](crate::daemon::audit::AuditLogger::log_record)
    /// swallows IO errors by §9's deliberate never-fail-the-hot-path design: a
    /// loss record that failed to write, followed by an envelope record that
    /// succeeded, would produce exactly the clean unexplained `delivered` line
    /// this ordering exists to prevent. Loss records therefore go through
    /// [`AuditLogger::try_log_record`](crate::daemon::audit::AuditLogger::try_log_record),
    /// and if ANY of them fails the envelope's own record is written with
    /// `losses_unrecorded: true` and an `error!` is emitted. A reader that sees
    /// that flag knows this delivery lost envelopes the stream cannot
    /// enumerate; a reader that does not see it knows the loss records for that
    /// `message_id` are complete. The hot path is still never failed — the
    /// sender gets its `202` either way, because the envelope did reach the
    /// clients that took it.
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
    /// `delivered_records_account_for_everything_a_stalled_subscriber_lost`,
    /// `eviction_is_recorded_per_client_in_the_durable_log`,
    /// `a_wedged_client_does_not_refuse_publishes_for_a_healthy_co_subscriber`,
    /// `a_publish_racing_deregistration_is_not_recorded_delivered`,
    /// `an_unwritable_log_never_reports_a_clean_delivery`,
    /// `concurrent_publishers_lose_nothing_without_a_record`.
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
            DeliveryOutcome::Delivered { missed } => {
                self.audit
                    .log_record(&EnvelopeRecord::new(&envelope, self.record_losses(&missed)));
                Ok(envelope)
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

/// The §9 line written for one published envelope.
///
/// Why a wrapper rather than a field on [`BusEnvelope`]: the envelope is
/// DOC-60 §11's schema and travels back to the sender over HTTP; whether this
/// daemon managed to write its companion loss records is a fact about the log,
/// not about the message. `serde(flatten)` means a delivery whose losses were
/// all recorded serializes byte-identically to the bare envelope, so §11
/// fidelity and every existing reader are unaffected — the extra key appears
/// only in the case it describes.
/// What: the envelope's own fields, plus `losses_unrecorded: true` when a
/// companion [`InboxMiss`] could not be written. See
/// [`PeerBus::record_losses`].
/// Test: `an_unwritable_log_never_reports_a_clean_delivery`,
/// `a_fully_recorded_delivery_serializes_as_a_bare_envelope`.
#[derive(Debug, serde::Serialize)]
pub struct EnvelopeRecord<'a> {
    /// The envelope, inlined so the line keeps §11's shape.
    #[serde(flatten)]
    pub envelope: &'a BusEnvelope,
    /// Present only when true: this delivery lost envelopes for one or more
    /// clients and at least one of those losses could not be written.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub losses_unrecorded: bool,
}

impl<'a> EnvelopeRecord<'a> {
    /// Wrap `envelope` for the §9 stream.
    pub fn new(envelope: &'a BusEnvelope, losses_unrecorded: bool) -> Self {
        Self {
            envelope,
            losses_unrecorded,
        }
    }
}

/// Shared handle the daemon state holds.
pub type SharedBus = Arc<PeerBus>;
