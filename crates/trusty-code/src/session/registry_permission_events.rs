//! `SessionRegistry`'s permission-prompt event plumbing and its
//! `PermissionEvents` implementation. Split out of `registry_events.rs`
//! (#8100) purely to keep that production file under the crate's 500-SLOC
//! cap — a child module of `registry` exactly as `events` is, so it shares
//! the same access to `SessionRegistry`'s private `record` helper.
//!
//! Why: `crate::permissions` publishes through a trait so it never depends on
//! the session layer; this is the session-side half of that seam.
//! What: `record_permission_requested`/`record_permission_resolved`, and the
//! `PermissionEvents` impl that forwards to them and answers
//! `prompter_attached`.
//! Test: `registry_tests::record_permission_requested_publishes_event`,
//! `registry_tests::record_permission_resolved_publishes_event`,
//! `registry_tests::prompter_attached_while_a_session_attachment_lives`.

use super::*;

impl SessionRegistry {
    /// Record a tool call suspended on an `ask` permission rule (#7948).
    ///
    /// Why: `crate::permissions` emits through its `PermissionEvents` trait so
    /// it never depends on the session layer; this is the implementation side.
    /// What: `subject` arrives already redacted and bounded by the gate and is
    /// recorded as-is.
    /// Test: `registry_tests::record_permission_requested_publishes_event`.
    #[allow(clippy::too_many_arguments)]
    pub fn record_permission_requested(
        &self,
        id: &str,
        request_id: &str,
        agent: &str,
        agent_id: &str,
        tool: &str,
        subject: &str,
        rule: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::PermissionRequested {
                session_id: id.to_string(),
                request_id: request_id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                tool: tool.to_string(),
                subject: subject.to_string(),
                rule: rule.to_string(),
            },
        );
        Ok(())
    }

    /// Record a suspended permission request reaching a decision (#7948).
    /// Test: `registry_tests::record_permission_resolved_publishes_event`.
    pub fn record_permission_resolved(
        &self,
        id: &str,
        request_id: &str,
        agent: &str,
        agent_id: &str,
        decision: &str,
        source: &str,
    ) -> Result<(), RpcError> {
        self.ensure_exists(id)?;
        self.record(
            id,
            Event::PermissionResolved {
                session_id: id.to_string(),
                request_id: request_id.to_string(),
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                decision: decision.to_string(),
                source: source.to_string(),
            },
        );
        Ok(())
    }
}

/// (#7948) The registry is the permission gate's event sink.
///
/// Why: `SessionRegistry::record` is the sole assigner of an event's `seq`;
/// implementing the trait here keeps `crate::permissions` free of a registry
/// dependency.
/// What: a `session_not_found` is logged at `debug` and dropped. It affects
/// only the event, never the decision: the gate still waits for an answer or
/// the timeout, and a timeout denies.
/// Test: `registry_tests::record_permission_requested_publishes_event`,
/// `registry_tests::record_permission_resolved_publishes_event`.
impl crate::permissions::PermissionEvents for SessionRegistry {
    fn permission_requested(
        &self,
        session_id: &str,
        request_id: &str,
        agent: &str,
        agent_id: &str,
        tool: &str,
        subject: &str,
        rule: &str,
    ) {
        if let Err(e) = self.record_permission_requested(
            session_id, request_id, agent, agent_id, tool, subject, rule,
        ) {
            tracing::debug!(session_id = %session_id, "record_permission_requested skipped: {e}");
        }
    }

    fn permission_resolved(
        &self,
        session_id: &str,
        request_id: &str,
        agent: &str,
        agent_id: &str,
        decision: &str,
        source: &str,
    ) {
        if let Err(e) = self
            .record_permission_resolved(session_id, request_id, agent, agent_id, decision, source)
        {
            tracing::debug!(session_id = %session_id, "record_permission_resolved skipped: {e}");
        }
    }

    /// Whether a prompter is watching THIS session and could answer its ask.
    ///
    /// Why (#8100): the signal must be per session. A daemon-global reading —
    /// `crate::events::bus().receiver_count()`, this method's first cut — is
    /// true for every session in the process as soon as one console attaches
    /// to one of them, so the normal trusty-mpm shape (a watched PM session
    /// dispatching headless `task.run` sub-agents) put every sub-agent back on
    /// the 300 s stall. `SessionRegistry::claim_prompter` counts only the
    /// paths bound to one session id that can BOTH see its
    /// `permission_requested` event and answer with
    /// `session.permission.respond`: `session.attach`, the `session.events`
    /// stream, and the per-session HTTP SSE route. The workstream-aggregate
    /// SSE route is an observer, not a prompter — it is bound to a workstream
    /// whose membership is re-checked per event, so no session's prompt is
    /// its to display — and deliberately claims nothing.
    /// What: `prompter_count` reads `0` for a session the registry cannot
    /// find, which denies. That is the fail-safe arm: a prompt for a session
    /// that does not exist is a prompt nobody can be shown.
    /// Test: `registry_tests::prompter_attached_while_a_session_attachment_lives`,
    /// `registry_tests::prompter_claim_is_released_when_its_guard_drops`,
    /// `registry_tests::prompter_attached_for_an_unknown_session_is_false`,
    /// `registry_tests::prompter_count_survives_a_poisoned_registry_lock`,
    /// `gate_tests::an_ask_on_an_unwatched_session_is_headless_while_a_sibling_is_watched`.
    fn prompter_attached(&self, session_id: &str) -> bool {
        self.prompter_count(session_id) > 0
    }
}
