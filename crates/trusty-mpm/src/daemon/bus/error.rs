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
    /// never-existed `404` without parsing the message.
    /// What: `404` not-found, `410` gone, `409` conflict, `400` bad request.
    /// Test: `bus_error_status_codes_map`, `instance_gone_is_410`.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NoLiveInstance { .. } => StatusCode::NOT_FOUND,
            Self::InstanceGone { .. } => StatusCode::GONE,
            Self::NoSubscriber { .. } => StatusCode::CONFLICT,
            Self::InvalidDefinitionId { .. } | Self::InvalidTarget(_) | Self::InvalidCaller(_) => {
                StatusCode::BAD_REQUEST
            }
        }
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
