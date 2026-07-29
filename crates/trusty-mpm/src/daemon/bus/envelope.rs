//! Peer-bus envelope schema (DOC-60 §11), carrying the §6 identity model.
//!
//! Why: DOC-60 §2's central finding is that all five existing buses address
//! the wrong unit — none of them carries the three identifiers (§6) that make
//! a message routable *and* attributable: the agent DEFINITION, the running
//! INSTANCE, and the per-message CALLER. This module is the one place those
//! three become a normative wire shape, so every producer and consumer agrees
//! on them instead of each inventing an ad hoc source tag.
//! What: [`BusEnvelope`] and its parts — [`BusEdge`], [`CallerIdentity`],
//! [`CallerKind`], [`ChannelOrigin`], [`Recipient`], [`DeliveryState`], and
//! [`BusPayload`]. Serde field names match DOC-60 §11's illustrative JSON so
//! the durable JSONL log (§9) is readable against the spec.
//! Test: `bus::tests` — `envelope_round_trips_json`,
//! `envelope_matches_doc60_field_names`, `peer_request_is_declinable`.
//!
//! Explicitly NOT here, all deferred per this change's scope: the durable
//! inbox and queue-not-spawn behavior (§7), cross-project addressing (§12 Q4),
//! retention policy (§12 Q1), and version-skew negotiation (§12 Q2/Q5).

use serde::{Deserialize, Serialize};

/// Which of DOC-60 §5's three edges an envelope crossed.
///
/// Why: the three edges share one envelope and one delivery path by design
/// (§5.3: "a lateral message and a user message look identical from the
/// receiving assistant's side"), so the edge is recorded as data rather than
/// encoded as three separate message types. Search (§9) and consolidation
/// (§10) need to tell them apart after the fact.
/// What: the three §5 edges, serialized in snake_case exactly as §11 spells
/// them.
/// Test: `envelope_matches_doc60_field_names`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusEdge {
    /// §5.1 — a user (or channel-originated user identity) and an assistant.
    UserAssistant,
    /// §5.2 — an assistant and an in-process sub-agent (recorded, not routed).
    AssistantSubagent,
    /// §5.3 — two Level-0 assistants, laterally. The edge this change serves.
    AssistantAssistant,
}

/// What kind of party sent a message (DOC-60 §6c).
///
/// Why: this is the field that structurally prevents a peer message from
/// being mistaken for a user message. ADR-0024's virtual-twin principle makes
/// that distinction load-bearing: a message from `AssistantInstance` carries
/// communication, never command, and a recipient must be able to see that
/// from the envelope alone.
/// What: the three §11 caller kinds.
/// Test: `envelope_matches_doc60_field_names`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerKind {
    /// A human acting directly (CLI/TUI/GUI). Mints no instance id.
    User,
    /// Another running assistant. Carries both §6a and §6b ids.
    AssistantInstance,
    /// A channel transport originating a user-identity message (§8).
    Channel,
}

impl CallerKind {
    /// The §5 edge a message from this caller crosses on the peer path.
    ///
    /// Why: [`BusEnvelope::edge`] must be DERIVED from who actually sent the
    /// message, never stamped as a constant. §9 search and §10 consolidation
    /// read `edge` to tell the three edges apart after the fact, so a
    /// hardcoded value would silently record §5.1 user traffic as §5.3 lateral
    /// traffic and corrupt exactly the distinction the field exists to carry.
    /// What: a TOTAL match over [`CallerKind`] — adding a kind is a compile
    /// error here until someone decides which edge it crosses, which is the
    /// property that keeps the derivation honest as §5.1/§5.2 land. Today the
    /// peer path carries [`CallerKind::AssistantInstance`] only; `user` and
    /// `channel` are rejected rather than mapped, because routing them here
    /// would let one assistant address another as a user (see
    /// [`BusError::CallerKindNotPermitted`](super::BusError::CallerKindNotPermitted)).
    /// When §5.1 lands it maps `User`/`Channel` to
    /// [`BusEdge::UserAssistant`] — a one-line change in this match, with no
    /// caller to update.
    /// Test: `peer_edge_derives_assistant_assistant`,
    /// `peer_edge_rejects_user_and_channel`,
    /// `published_envelope_edge_is_derived`.
    pub fn peer_edge(self) -> Result<BusEdge, super::BusError> {
        match self {
            Self::AssistantInstance => Ok(BusEdge::AssistantAssistant),
            Self::User | Self::Channel => {
                Err(super::BusError::CallerKindNotPermitted { kind: self })
            }
        }
    }
}

/// Platform-native identity behind a channel-originated message (DOC-60 §8).
///
/// Why: §8 requires all three values be carried separately and "none collapsed
/// into one" — a channel message is not anonymous the way a bare CLI keystroke
/// is, and flattening them to a string would destroy the tenant
/// disambiguation the RBAC layer needs.
/// What: connector name plus the platform's sender, conversation, and tenant
/// ids.
/// Test: `envelope_round_trips_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelOrigin {
    /// Connector name, e.g. `"slack"` or `"telegram"`.
    pub connector: String,
    /// Platform-native sender id — the accountable human.
    pub human_sender: String,
    /// Platform conversation id, needed to route a reply back.
    pub channel: String,
    /// Platform tenant/workspace id, needed to disambiguate across tenants.
    pub workspace: String,
}

/// Who sent this message (DOC-60 §6c, one per envelope).
///
/// Why: caller identity is per-message, not session-lived — the same instance
/// can receive from a user on one envelope and a peer on the next, and the
/// reply must be addressable back to whichever asked.
/// What: the kind tag plus the §6a/§6b ids when the caller is an agent, and
/// the §8 channel identity when it is a channel.
/// Test: `envelope_round_trips_json`, `caller_validation_rejects_bare_peer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerIdentity {
    /// Which kind of party sent it.
    pub kind: CallerKind,
    /// §6b — the sending assistant's instance id, when `kind` is
    /// [`CallerKind::AssistantInstance`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// §6a — the sending agent's definition id, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_id: Option<String>,
    /// §8 — present only when `kind` is [`CallerKind::Channel`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_origin: Option<ChannelOrigin>,
}

impl CallerIdentity {
    /// Reject a caller identity whose ids contradict its declared kind.
    ///
    /// Why: an `assistant_instance` caller with no `instance_id` would let a
    /// peer message masquerade as an unattributable one, defeating the whole
    /// point of §6c.
    ///
    /// **This is a STRUCTURAL check only — it proves nothing about identity.**
    /// It confirms the declared kind and the supplied ids are mutually
    /// consistent; it cannot tell whether the sender is who it claims, because
    /// the fields are entirely client-asserted. Attributability of the §9 log
    /// comes from
    /// [`PeerBus::publish`](super::PeerBus::publish) resolving the claimed
    /// `instance_id` against a live registration and re-stamping the
    /// definition from the registry. Do not read this method as an
    /// authentication step.
    /// What: requires `instance_id` for [`CallerKind::AssistantInstance`] and
    /// `channel_origin` for [`CallerKind::Channel`]; [`CallerKind::User`]
    /// requires neither.
    /// Test: `caller_validation_rejects_bare_peer`,
    /// `caller_validation_accepts_user`.
    pub fn validate(&self) -> Result<(), super::BusError> {
        match self.kind {
            CallerKind::AssistantInstance if self.instance_id.is_none() => {
                Err(super::BusError::InvalidCaller(
                    "kind=assistant_instance requires instance_id (DOC-60 §6c)".into(),
                ))
            }
            CallerKind::Channel if self.channel_origin.is_none() => {
                Err(super::BusError::InvalidCaller(
                    "kind=channel requires channel_origin (DOC-60 §8)".into(),
                ))
            }
            _ => Ok(()),
        }
    }
}

/// The resolved recipient stamped onto a delivered envelope (DOC-60 §11 `to`).
///
/// Why: §11 says `definition_id` is "always present" on `to`. Instance bypass
/// (a sender addressing a live `instance_id` directly) does not weaken that:
/// bypass is a REQUEST-time addressing mode, and the daemon fills BOTH ids
/// from the registry before stamping the envelope. The one case where
/// `definition_id` is absent is a *dropped* bypass record — the instance was
/// already gone, so the daemon has nothing to resolve its definition from.
/// What: the resolved instance id and the definition it belongs to.
/// Test: `bypass_publish_stamps_both_ids`, `dropped_bypass_has_no_definition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    /// §6b — the live instance the envelope was routed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// §6a — the definition that instance runs. Absent only on a dropped
    /// bypass record, where no live instance existed to resolve it from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_id: Option<String>,
}

/// An envelope's own delivery lifecycle (DOC-60 §11).
///
/// Why: ADR-0019's acknowledgment requirement, carried into this bus. The
/// durable log must record a failed delivery as emphatically as a successful
/// one — that record is what makes "never sent" distinguishable from "sent but
/// never read".
/// What: the four §11 states. This change produces only `Delivered` and
/// `Dropped`; `Queued` and `Acked` await §7's durable inbox and the client-side
/// ack path respectively, and are defined now so the log format does not
/// change when they land.
/// Test: `failed_publish_logs_dropped_envelope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// Held in a durable inbox awaiting the recipient (DOC-60 §7 — deferred).
    Queued,
    /// Handed to the recipient's subscriber channel.
    Delivered,
    /// The recipient confirmed receipt (client-side ack — deferred).
    Acked,
    /// Delivery failed; the envelope is logged but reached no one.
    Dropped,
}

/// What an envelope actually carries.
///
/// Why: DOC-60 §5.3 makes decline-ability normative, derived from ADR-0024's
/// virtual-twin authority principle: a peer message that REQUESTS action must
/// be decline-able, or it is delegation under another name. That requires the
/// wire shape to distinguish a request from a plain informational message —
/// which is precisely why [`BusPayload::PeerRequest`] and
/// [`BusPayload::PeerResponse`] are separate variants rather than one
/// `chat_text` blob with a convention layered on top. §5.3 leaves the exact
/// shape to a later normative revision and settles only that decline-ability
/// is required; this is the minimum shape that satisfies it.
/// What: three variants, externally tagged by a `type` field per §11, seeded
/// from the `events.rs` vocabulary §3 retirement #4 names as the seed set.
/// Test: `peer_request_is_declinable`, `envelope_round_trips_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BusPayload {
    /// Informational text. Carries no request for action, so nothing to
    /// decline.
    ChatText {
        /// The message body.
        text: String,
    },
    /// A request for the recipient to act. MUST be answerable with a
    /// [`BusPayload::PeerResponse`] carrying `accepted: false`.
    PeerRequest {
        /// What is being asked.
        text: String,
    },
    /// The recipient's answer to a [`BusPayload::PeerRequest`].
    PeerResponse {
        /// `false` is always a legitimate answer — this is the field that
        /// keeps the lateral edge communication rather than command.
        accepted: bool,
        /// The recipient's reasoning or result.
        text: String,
    },
}

/// One addressed, durable message crossing the peer bus (DOC-60 §11).
///
/// Why: this is the unit §9's JSONL log stores, §10's provenance pointer
/// references by `message_id`, and §5's three edges all share. Making it one
/// type — rather than one per edge — is what lets a receiving assistant handle
/// a user message and a peer message through a single path.
/// What: the §11 field set, with `message_id` globally unique and stable
/// across log rotation so a promoted memory can point back at it (§10).
/// Test: `envelope_round_trips_json`, `envelope_matches_doc60_field_names`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusEnvelope {
    /// Globally unique, stable id — the §10 provenance key.
    pub message_id: String,
    /// RFC3339 stamp of when the daemon accepted the message.
    pub ts: String,
    /// The `message_id` this replies to, threading a conversation for §9's
    /// `/bus/thread` walk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    /// Which §5 edge this crossed.
    pub edge: BusEdge,
    /// Who sent it (§6c).
    pub from: CallerIdentity,
    /// Who it was routed to (§6a/§6b).
    pub to: Recipient,
    /// This envelope's own lifecycle.
    pub delivery_state: DeliveryState,
    /// The message body.
    pub payload: BusPayload,
}

impl BusEnvelope {
    /// Mint a new envelope with a fresh id and timestamp.
    ///
    /// Why: `message_id` and `ts` are daemon-assigned, never client-supplied —
    /// a client-chosen id could collide or be forged, and a client-chosen
    /// timestamp would make the append-only log non-monotonic. Centralizing
    /// the mint keeps both properties true by construction.
    /// What: stamps a v4 UUID and the current UTC time, then copies the
    /// caller-supplied routing and payload fields in.
    /// Test: `envelope_ids_are_unique`, `envelope_round_trips_json`.
    pub fn new(
        edge: BusEdge,
        from: CallerIdentity,
        to: Recipient,
        payload: BusPayload,
        in_reply_to: Option<String>,
        delivery_state: DeliveryState,
    ) -> Self {
        Self {
            message_id: uuid::Uuid::new_v4().to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            in_reply_to,
            edge,
            from,
            to,
            delivery_state,
            payload,
        }
    }
}
