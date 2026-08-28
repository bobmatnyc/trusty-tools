//! Peer-bus error contract (DOC-60 §4 fail-closed).
//!
//! Why: DOC-60 §4 is explicit that a message the bus cannot deliver returns an
//! explicit error to the caller "rather than silently dropping", because a
//! silent drop recreates the exact failure mode ADR-0019 was written to
//! eliminate — no way to distinguish "never sent" from "sent but never read".
//! Every way delivery can fail therefore gets a named variant here, and the
//! HTTP mapping is chosen so a client can branch on the status alone.
//! What: [`BusError`], a `thiserror` enum with one variant per delivery
//! failure, plus its `axum::IntoResponse` mapping.
//! Test: `bus_error_status_codes_map`, `instance_gone_is_410`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;
use trusty_common::uds::server::{CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS, RpcError};

use crate::daemon::error::{CODE_CONFLICT, CODE_FORBIDDEN, CODE_GONE, CODE_NOT_FOUND};

/// A peer-bus publish, addressing, or registration failure.
///
/// Why: the sender must be able to tell *which* of the three distinguishable
/// bad outcomes it hit — the definition has nothing running, the specific
/// instance it named is gone, or the instance is registered but has no
/// attached subscriber — because the correct client reaction differs for each
/// (start one / re-resolve the definition / retry).
/// What: four addressing/delivery variants plus two request-validation ones.
/// Test: `bus_error_status_codes_map`, `instance_gone_is_410`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BusError {
    /// Definition-addressed delivery found no live instance (DOC-60 §5.3).
    ///
    /// The durable inbox that would queue this message is DOC-60 §7 work and
    /// is deliberately not built here, so the MVP fails closed instead.
    #[error(
        "no live instance registered for definition '{definition_id}' \
         (durable inbox queueing is DOC-60 §7, not implemented)"
    )]
    NoLiveInstance {
        /// The definition id that resolved to nothing.
        definition_id: String,
    },

    /// Instance-bypass delivery named an `instance_id` that is no longer live.
    ///
    /// This is the failure the bypass mode exists to surface: the sender is
    /// told its specific target died, and is NOT silently redirected to some
    /// other instance of the same definition. See [`super::registry`] for the
    /// full rationale.
    #[error(
        "instance '{instance_id}' is no longer live; the sender's specific \
         target is gone and was NOT redirected to another instance of its \
         definition — re-resolve by definition_id if any instance will do"
    )]
    InstanceGone {
        /// The instance id the sender held and targeted directly.
        instance_id: String,
    },

    /// The target instance is registered but has no attached subscriber.
    ///
    /// Why this is not a silent success: the envelope would be dropped on the
    /// floor. DOC-60 §7's durable inbox is what will turn this into a queue;
    /// until then the sender is told, per §4's fail-closed rule.
    #[error(
        "instance '{instance_id}' is registered but has no attached \
         subscriber; the message was not delivered"
    )]
    NoSubscriber {
        /// The registered-but-unattached instance id.
        instance_id: String,
    },

    /// The sender claimed an `instance_id` that is not a live registration.
    ///
    /// Why this is 403-class and NOT a reuse of [`BusError::InstanceGone`]:
    /// the two describe opposite ends of the message. `InstanceGone` (410) is
    /// about the RECIPIENT — "your target died, re-address it" — and a client's
    /// correct recovery is to re-resolve by `definition_id`. This variant is
    /// about the SENDER — "you are claiming an identity the registry cannot
    /// confirm" — and the correct recovery is for the sender to register
    /// itself. Collapsing them into one variant would leave a client unable to
    /// tell which end of its own request was at fault, which is precisely the
    /// ambiguity DOC-60 §4's explicit-error rule exists to prevent.
    #[error(
        "sender instance '{instance_id}' is not a live registration; a peer \
         message must be sent by a registered instance (DOC-60 §6b/§6c)"
    )]
    UnregisteredSender {
        /// The unverifiable instance id the sender claimed.
        instance_id: String,
    },

    /// A caller kind that this delivery path does not carry.
    ///
    /// Why: the peer path serves DOC-60 §5.3's lateral edge only. Accepting a
    /// `user`-kind caller here would let one assistant hand another a message
    /// that the recipient reads as a user instruction — assistant-to-assistant
    /// delegation reconstituted through the very bus ADR-0024 closed it from.
    #[error(
        "caller kind {kind:?} may not publish on the peer path; DOC-60 §5.3 \
         carries assistant_instance senders only (§5.1/§5.2 edges are not \
         routed here)"
    )]
    CallerKindNotPermitted {
        /// The rejected caller kind.
        kind: super::envelope::CallerKind,
    },

    /// A definition id failed validation.
    #[error("invalid definition id '{definition_id}': {reason}")]
    InvalidDefinitionId {
        /// The rejected definition id.
        definition_id: String,
        /// Why it was rejected.
        reason: String,
    },

    /// The publish request named neither an instance nor a definition.
    #[error("invalid target: {0}")]
    InvalidTarget(String),

    /// The caller identity on the envelope was inconsistent (DOC-60 §6c).
    #[error("invalid caller identity: {0}")]
    InvalidCaller(String),
}

impl BusError {
    /// The HTTP status this failure maps to.
    ///
    /// Why: separated from `IntoResponse` so tests can assert the mapping
    /// without constructing a response body. `410 Gone` is chosen for
    /// [`BusError::InstanceGone`] deliberately — it is the one status whose
    /// HTTP semantics ("the target existed and no longer does") match the
    /// bypass failure mode exactly, so a client can distinguish it from a
    /// never-existed `404` without parsing the message. `403` is reserved for
    /// [`BusError::UnregisteredSender`] — the one failure about the caller's
    /// own identity rather than the target's.
    /// What: `403` sender unverified, `404` not-found, `410` gone, `409`
    /// conflict, `400` bad request.
    /// Test: `bus_error_status_codes_map`, `instance_gone_is_410`,
    /// `unregistered_sender_is_403`.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::UnregisteredSender { .. } => StatusCode::FORBIDDEN,
            Self::NoLiveInstance { .. } => StatusCode::NOT_FOUND,
            Self::InstanceGone { .. } => StatusCode::GONE,
            Self::NoSubscriber { .. } => StatusCode::CONFLICT,
            Self::InvalidDefinitionId { .. }
            | Self::InvalidTarget(_)
            | Self::InvalidCaller(_)
            | Self::CallerKindNotPermitted { .. } => StatusCode::BAD_REQUEST,
        }
    }
}

/// Map a bus failure onto the JSON-RPC error frame a socket client reads.
///
/// Why (#6288 slice 5): the bus's four request/response verbs are served over
/// the Unix socket as well as over HTTP, and DOC-60 §4's whole point is that a
/// sender can tell WHICH failure it hit from the status alone. A socket caller
/// gets the same discrimination only if the code is derived from the same
/// [`BusError::status`] table the HTTP transport reads — a second hand-written
/// table here would let the two drift, and the drift would be silent.
/// What: derives the code from the status, so `403` sender-unverified, `404`
/// no-live-instance, `410` instance-gone, `409` no-subscriber, and `400`
/// malformed each stay distinguishable. The message crosses verbatim, so it is
/// the same string the HTTP body's `error` field carries.
/// Test: `bus_error_rpc_codes_track_http_statuses`.
impl From<BusError> for RpcError {
    fn from(e: BusError) -> Self {
        let code = match e.status() {
            StatusCode::BAD_REQUEST => CODE_INVALID_PARAMS,
            StatusCode::FORBIDDEN => CODE_FORBIDDEN,
            StatusCode::NOT_FOUND => CODE_NOT_FOUND,
            StatusCode::CONFLICT => CODE_CONFLICT,
            StatusCode::GONE => CODE_GONE,
            // Unreachable while `status` stays total over the variants above;
            // a new variant that forgets a row lands here rather than silently
            // borrowing another failure's code.
            _ => CODE_INTERNAL_ERROR,
        };
        RpcError::new(code, e.to_string())
    }
}

impl IntoResponse for BusError {
    fn into_response(self) -> Response {
        let status = self.status();
        (
            status,
            Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}
