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
//! `registry_tests::prompter_attached_while_an_event_consumer_lives`.

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

    /// Whether anything is consuming the event bus a prompt travels on.
    ///
    /// Why (#8100): every way a client can see an `Event::PermissionRequested`
    /// — the `session.attach` forwarder, the `session.events` stream, and the
    /// HTTP SSE route — holds a `crate::events::subscribe()` receiver for as
    /// long as it is live, so a zero receiver count means the prompt would be
    /// published into a bus with no readers. Reading the bus rather than this
    /// registry's own `attachments` map is deliberate: the two streaming
    /// clients replay without attaching, and counting only attachments would
    /// deny an operator who IS watching over SSE.
    /// What: the count is process-wide, not per session, so a client watching a
    /// DIFFERENT session still reads as attached. That error is in the safe
    /// direction — it keeps the pre-#8100 wait, and never turns a watched
    /// `ask` into a surprise deny.
    /// Test: `registry_tests::prompter_attached_while_an_event_consumer_lives`.
    /// The bus is process-global, so the no-consumer direction cannot be
    /// asserted from a test binary that runs tests in parallel; the gate's own
    /// arms cover it with a sink that answers directly
    /// (`ask_without_an_attached_prompter_denies_immediately`).
    fn prompter_attached(&self, _session_id: &str) -> bool {
        crate::events::bus().receiver_count() > 0
    }
}
