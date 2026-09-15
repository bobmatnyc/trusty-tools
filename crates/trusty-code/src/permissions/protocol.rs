//! The `session.permission.respond` JSON-RPC method and its wire types
//! (#7948).
//!
//! Why: an `ask` rule suspends a tool call inside a spawned task; a client
//! releases it with this request. It lives beside the gate because it writes
//! into the permission broker, not the session registry.
//! What: `PermissionDecision` (the three answers), `PermissionRespondParams`
//! (the flat wire shape), and `register`.
//!
//! Trust model (#7948): `session.permission.respond` accepts an answer for ANY
//! session from ANY connection that can reach the daemon's RPC surface. It
//! checks neither that the caller is attached to the session nor who the
//! caller is. This matches every other RPC method today. Revisit it before the
//! daemon serves more than one tenant.
//! Test: `permissions::tests::protocol_tests` — the whole module.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::session::PermissionBroker;

/// The JSON-RPC method name clients call to answer a permission request.
pub const PERMISSION_RESPOND_METHOD: &str = "session.permission.respond";

/// A client's answer to one `Event::PermissionRequested`.
///
/// Why: "yes, once" and "yes, stop asking for this" are the pair every
/// interactive prompt offers; collapsing them either nags or over-grants.
/// What: `AllowForSession.pattern` is the client's choice of grant width;
/// absent, the grant covers exactly the subject that was asked about. Grants
/// die with the session.
/// Test: `decision_from_params_maps_every_word`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    AllowForSession { pattern: Option<String> },
    Deny,
}

impl PermissionDecision {
    /// The stable wire word for this decision.
    /// Test: `decision_from_params_maps_every_word`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AllowOnce => "allow_once",
            Self::AllowForSession { .. } => "allow_for_session",
            Self::Deny => "deny",
        }
    }
}

/// `session.permission.respond`'s parameters.
///
/// What: `session_id`/`request_id` come off the rendered
/// `Event::PermissionRequested`; `decision` is `allow_once` |
/// `allow_for_session` | `deny`; `pattern` applies only to `allow_for_session`.
/// Test: `respond_rejects_an_unknown_decision_word`,
/// `respond_resolves_a_pending_request`.
#[derive(Debug, Clone, Deserialize)]
pub struct PermissionRespondParams {
    pub session_id: String,
    pub request_id: String,
    pub decision: String,
    #[serde(default)]
    pub pattern: Option<String>,
}

impl PermissionRespondParams {
    /// Map the flat wire words onto a [`PermissionDecision`]; an unknown word
    /// is `-32602`, never a guessed allow or deny.
    /// Test: `decision_from_params_maps_every_word`,
    /// `respond_rejects_an_unknown_decision_word`.
    pub fn decision(&self) -> Result<PermissionDecision, RpcError> {
        match self.decision.as_str() {
            "allow_once" => Ok(PermissionDecision::AllowOnce),
            "allow_for_session" => Ok(PermissionDecision::AllowForSession {
                pattern: self.pattern.clone(),
            }),
            "deny" => Ok(PermissionDecision::Deny),
            other => Err(RpcError::invalid_params(format!(
                "unknown permission decision '{other}' — expected one of \
                 allow_once, allow_for_session, deny"
            ))),
        }
    }
}

/// Register `session.permission.respond` onto `router`.
///
/// Why: one call in `crate::serve::build_router`, closed over the daemon's
/// single broker.
/// What: parses the params, maps the decision, and hands it to the broker. A
/// request id nobody waits on (already answered, or timed out) is `-32602`.
/// Test: `register_wires_the_respond_method`,
/// `respond_rejects_an_unknown_request_id`.
pub fn register(router: &mut Router, broker: Arc<PermissionBroker>) {
    router.register(
        PERMISSION_RESPOND_METHOD,
        move |params: Value, _ctx: ConnectionContext| {
            let broker = Arc::clone(&broker);
            async move {
                let params: PermissionRespondParams =
                    serde_json::from_value(params).map_err(|e| {
                        RpcError::invalid_params(format!("{PERMISSION_RESPOND_METHOD}: {e}"))
                    })?;
                let decision = params.decision()?;
                broker.respond(&params.session_id, &params.request_id, decision)?;
                Ok(json!({"accepted": true}))
            }
        },
    );
}
