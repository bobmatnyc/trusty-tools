//! Per-session permission state and the daemon-wide broker (#7948).
//!
//! Why: an `ask` suspends a tool call inside a spawned task, while the answer
//! arrives on a JSON-RPC connection that knows only a session id. Both sides
//! need one shared rendezvous, and `allow_for_session` grants must live exactly
//! as long as the session that granted them.
//! What: `SessionPermissions` (pending requests plus remembered grants) and
//! `PermissionBroker` (session id -> `SessionPermissions`).
//! Test: `permissions::tests::gate_tests`, `permissions::tests::protocol_tests`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::jsonrpc::RpcError;

use super::config::CompiledGlob;
use super::protocol::PermissionDecision;

/// What a remembered `allow_for_session` grant covers.
///
/// Why: a bare grant covers exactly the subject that was asked about, and a
/// client pattern covers a glob. Keeping them separate avoids escaping a
/// literal into a glob, which is easy to get wrong in the widening direction.
enum AllowanceSubject {
    /// A grant for a call with no subject (an MCP tool).
    Any,
    Literal(String),
    Glob(CompiledGlob),
}

impl AllowanceSubject {
    fn matches(&self, subject: Option<&str>) -> bool {
        match (self, subject) {
            (Self::Any, _) => true,
            (Self::Literal(literal), Some(s)) => literal == s,
            (Self::Glob(glob), Some(s)) => glob.is_match(s),
            (Self::Literal(_) | Self::Glob(_), None) => false,
        }
    }
}

/// One remembered `allow_for_session` grant.
struct Allowance {
    tool: String,
    subject: AllowanceSubject,
}

/// One session's live permission state.
///
/// Why: reached from the agent loop (which waits) and from
/// `session.permission.respond` (which answers); sharing one `Arc` is the
/// whole mechanism.
/// What: `pending` maps a request id to the sender the waiting gate holds;
/// `remembered` holds this session's grants and is never shared across
/// sessions. A poisoned lock never widens: registration drops the sender (the
/// waiting gate resolves to deny) and a remembered-grant lookup reports "not
/// remembered" (the call is asked about again).
/// Test: `allow_for_session_is_remembered_for_a_later_matching_call`,
/// `allow_for_session_does_not_leak_across_sessions`,
/// `abandoned_request_resolves_to_deny`.
#[derive(Default)]
pub struct SessionPermissions {
    pending: Mutex<HashMap<String, oneshot::Sender<PermissionDecision>>>,
    remembered: Mutex<Vec<Allowance>>,
}

impl SessionPermissions {
    /// Register a request and hand back the receiver the gate awaits.
    pub(crate) fn register(&self, request_id: &str) -> oneshot::Receiver<PermissionDecision> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(request_id.to_string(), tx);
        }
        rx
    }

    /// Drop a request that is no longer awaited (timeout path).
    pub(crate) fn forget(&self, request_id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(request_id);
        }
    }

    /// Deliver a client's answer to the waiting gate.
    ///
    /// Why: a request id nobody waits on (already answered, or timed out) is a
    /// client error, not an internal one.
    /// Test: `respond_to_unknown_request_is_an_error`,
    /// `respond_resolves_the_waiting_gate`.
    pub fn respond(&self, request_id: &str, decision: PermissionDecision) -> Result<(), RpcError> {
        let sender = self
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(request_id));
        let Some(sender) = sender else {
            return Err(RpcError::invalid_params(format!(
                "no pending permission request with id '{request_id}'"
            )));
        };
        sender.send(decision).map_err(|_| {
            RpcError::internal(format!(
                "permission request '{request_id}' is no longer awaited"
            ))
        })
    }

    /// Whether earlier grants cover this call — EVERY subject must be covered.
    pub(crate) fn is_remembered(&self, tool: &str, subjects: &[String]) -> bool {
        let Ok(remembered) = self.remembered.lock() else {
            return false;
        };
        let covered = |subject: Option<&str>| {
            remembered
                .iter()
                .any(|a| a.tool == tool && a.subject.matches(subject))
        };
        if subjects.is_empty() {
            covered(None)
        } else {
            subjects.iter().all(|s| covered(Some(s)))
        }
    }

    /// Record an `allow_for_session` grant.
    ///
    /// Why: `pattern` is the client's choice of how wide the grant is. An
    /// absent pattern grants exactly this call's subjects and nothing else.
    /// What: an uncompilable pattern is dropped with a warning — the approval
    /// still stands for THIS call, it is simply not remembered.
    /// Test: `bare_allow_for_session_grants_only_that_subject`,
    /// `allow_for_session_pattern_grants_the_glob`,
    /// `unusable_allow_for_session_pattern_remembers_nothing`.
    pub(crate) fn remember(&self, tool: &str, subjects: &[String], pattern: Option<&str>) {
        let grants: Vec<AllowanceSubject> = match pattern {
            Some(p) => match CompiledGlob::compile(p) {
                Ok(glob) => vec![AllowanceSubject::Glob(glob)],
                Err(reason) => {
                    tracing::warn!(
                        tool = %tool,
                        pattern = %p,
                        "permission: allow-for-session pattern is not a valid glob ({reason}) — \
                         allowing this call but remembering nothing"
                    );
                    return;
                }
            },
            None if subjects.is_empty() => vec![AllowanceSubject::Any],
            None => subjects
                .iter()
                .map(|s| AllowanceSubject::Literal(s.clone()))
                .collect(),
        };
        if let Ok(mut remembered) = self.remembered.lock() {
            remembered.extend(grants.into_iter().map(|subject| Allowance {
                tool: tool.to_string(),
                subject,
            }));
        }
    }
}

/// The daemon-wide map of per-session permission state.
///
/// Why: the rendezvous between a waiting gate and `session.permission.respond`.
/// Built once in `crate::serve::build_router` and cloned where needed (like
/// `SharedWorkstreamStore`) rather than a process-global, so a test builds its own.
/// Test: `permissions::tests::protocol_tests`.
#[derive(Default)]
pub struct PermissionBroker {
    sessions: Mutex<HashMap<String, Arc<SessionPermissions>>>,
}

/// Opaque `Debug`: reports only how many sessions hold state, so formatting a
/// struct that carries the broker never prints an unapproved subject.
impl std::fmt::Debug for PermissionBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sessions = self.sessions.lock().map(|s| s.len()).unwrap_or(0);
        f.debug_struct("PermissionBroker")
            .field("sessions", &sessions)
            .finish()
    }
}

impl PermissionBroker {
    /// Construct an empty broker.
    pub fn new() -> Self {
        Self::default()
    }

    /// This session's state, created on first use, so a session that never
    /// asks never allocates.
    /// Test: `broker_hands_the_same_state_back_for_one_session`.
    pub fn session(&self, session_id: &str) -> Arc<SessionPermissions> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            sessions
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(SessionPermissions::default())),
        )
    }

    /// Answer a pending request for `session_id`.
    /// Test: `respond_to_unknown_session_is_an_error`.
    pub fn respond(
        &self,
        session_id: &str,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<(), RpcError> {
        let existing = self
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(session_id).cloned());
        match existing {
            Some(state) => state.respond(request_id, decision),
            None => Err(RpcError::invalid_params(format!(
                "session '{session_id}' has no pending permission requests"
            ))),
        }
    }
}
