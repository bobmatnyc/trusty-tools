//! Concrete `ToolEventSink` forwarding tool-dispatch hooks to
//! `SessionRegistry::record_tool_*` (#2056).
//!
//! Why: #2055 built the emission plumbing; #2056's `agent_loop` extension
//! built the hook (`agent_loop::ToolEventSink`) that CAN call it. This is the
//! small piece of glue that actually wires the two together for a specific
//! session, kept in `task` (the daemon-execution layer) rather than
//! `agent_loop` (the generic engine) or `session` (which must not depend
//! upward on the engine) — see `agent_loop::sink`'s module docs for the
//! layering rationale.
//! What: [`SessionToolEventSink`] holds an `Arc<SessionRegistry>` and the
//! target `session_id`; each hook forwards to the matching `record_tool_*`
//! call, logging (not propagating — the sink trait's methods return `()`)
//! any failure, which in practice only happens if the session vanished
//! mid-run (e.g. a race with daemon shutdown).
//! Test: `task::sink::tests::*`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent_loop::ToolEventSink;
use crate::session::SessionRegistry;

/// Forwards `AgentLoop` tool-dispatch hooks to a specific session's
/// `SessionRegistry::record_tool_*` calls (#2056).
///
/// Why: the seam that makes a daemon-driven run's tool activity observable
/// to a `session.attach`ed client, over both the PM's own loop and any
/// delegated sub-agent loop that shares this same `Arc`.
/// What: See module docs.
/// Test: `task::sink::tests::forwards_started_finished_and_error`.
pub struct SessionToolEventSink {
    registry: Arc<SessionRegistry>,
    session_id: String,
}

impl SessionToolEventSink {
    /// Build a sink targeting `session_id` on `registry`.
    pub fn new(registry: Arc<SessionRegistry>, session_id: impl Into<String>) -> Self {
        Self {
            registry,
            session_id: session_id.into(),
        }
    }
}

#[async_trait]
impl ToolEventSink for SessionToolEventSink {
    async fn tool_started(&self, call_id: &str, tool: &str, args_preview: &str) {
        if let Err(e) =
            self.registry
                .record_tool_started(&self.session_id, tool, call_id, args_preview)
        {
            tracing::warn!(session_id = %self.session_id, tool, call_id, "record_tool_started failed: {e}");
        }
    }

    async fn tool_finished(&self, call_id: &str, tool: &str, success: bool, result_preview: &str) {
        if let Err(e) = self.registry.record_tool_finished(
            &self.session_id,
            tool,
            call_id,
            success,
            result_preview,
        ) {
            tracing::warn!(session_id = %self.session_id, tool, call_id, "record_tool_finished failed: {e}");
        }
    }

    async fn tool_error(&self, call_id: &str, tool: &str, error: &str) {
        if let Err(e) = self
            .registry
            .record_tool_error(&self.session_id, tool, call_id, error)
        {
            tracing::warn!(session_id = %self.session_id, tool, call_id, "record_tool_error failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    /// Read the process-global event bus until an envelope matching
    /// `session_id` arrives (mirrors `session::registry::registry_tests`'
    /// helper — the global bus is shared across the whole test binary).
    async fn next_event_for(
        rx: &mut broadcast::Receiver<crate::events::SessionEventEnvelope>,
        session_id: &str,
    ) -> crate::events::SessionEventEnvelope {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let envelope = rx.recv().await.expect("event bus closed unexpectedly");
                if envelope.session_id == session_id {
                    return envelope;
                }
            }
        })
        .await
        .expect("timed out waiting for an event on this session")
    }

    /// Each hook must forward to the matching `record_tool_*` call and
    /// publish the corresponding #2055 event kind.
    #[tokio::test]
    async fn forwards_started_finished_and_error() {
        let registry = Arc::new(SessionRegistry::new());
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let sink = SessionToolEventSink::new(Arc::clone(&registry), session.id.clone());
        let mut events = crate::events::subscribe();

        sink.tool_started("call-1", "bash", "echo hi").await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "tool_started");

        sink.tool_finished("call-1", "bash", true, "hi").await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "tool_finished");

        sink.tool_error("call-1", "bash", "boom").await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "tool_error");
    }

    /// Hooks against an unknown session must not panic (they log and
    /// swallow the `session_not_found` error — the sink trait is `()`-only).
    #[tokio::test]
    async fn unknown_session_does_not_panic() {
        let registry = Arc::new(SessionRegistry::new());
        let sink = SessionToolEventSink::new(registry, "does-not-exist".to_string());
        sink.tool_started("c", "bash", "x").await;
        sink.tool_finished("c", "bash", true, "x").await;
        sink.tool_error("c", "bash", "x").await;
    }
}
