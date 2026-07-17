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

use crate::agent_loop::{ContextBudgetSnapshot, ToolEventSink};
use crate::session::SessionRegistry;
use crate::tools::telemetry::ToolTelemetry;

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
    async fn tool_started(&self, agent: &str, call_id: &str, tool: &str, args_preview: &str) {
        if let Err(e) =
            self.registry
                .record_tool_started(&self.session_id, agent, tool, call_id, args_preview)
        {
            tracing::warn!(session_id = %self.session_id, agent, tool, call_id, "record_tool_started failed: {e}");
        }
    }

    async fn tool_finished(
        &self,
        agent: &str,
        call_id: &str,
        tool: &str,
        success: bool,
        result_preview: &str,
    ) {
        if let Err(e) = self.registry.record_tool_finished(
            &self.session_id,
            agent,
            tool,
            call_id,
            success,
            result_preview,
        ) {
            tracing::warn!(session_id = %self.session_id, agent, tool, call_id, "record_tool_finished failed: {e}");
        }
    }

    async fn tool_error(&self, agent: &str, call_id: &str, tool: &str, error: &str) {
        if let Err(e) =
            self.registry
                .record_tool_error(&self.session_id, agent, tool, call_id, error)
        {
            tracing::warn!(session_id = %self.session_id, agent, tool, call_id, "record_tool_error failed: {e}");
        }
    }

    /// Fan a tool's structured telemetry out to the matching #UI-Phase-1
    /// `record_*` call.
    ///
    /// Why: the variant-to-record mapping belongs here, in the daemon-glue
    /// layer, rather than in `agent_loop` (which must not know about
    /// `SessionRegistry`) or in the tools (which must not know about
    /// sessions at all).
    /// What: same log-and-swallow contract as the hooks above — a vanished
    /// session must never derail a run.
    /// Test: `tests::forwards_search_and_recall_telemetry`.
    async fn tool_telemetry(
        &self,
        agent: &str,
        call_id: &str,
        tool: &str,
        telemetry: &ToolTelemetry,
    ) {
        let recorded = match telemetry {
            ToolTelemetry::Search(t) => {
                self.registry
                    .record_search_performed(&self.session_id, agent, t)
            }
            ToolTelemetry::Recall(t) => {
                self.registry
                    .record_memory_recalled(&self.session_id, agent, t)
            }
        };
        if let Err(e) = recorded {
            tracing::warn!(session_id = %self.session_id, agent, tool, call_id, "record tool telemetry failed: {e}");
        }
    }

    /// Forward the per-turn working-context measurement to this session's
    /// event stream (epic #2343). Same log-and-swallow contract as the tool
    /// hooks above: a vanished session must never fault the PM's turn
    /// boundary — telemetry is not worth failing the work it observes.
    async fn context_budget(&self, snapshot: &ContextBudgetSnapshot) {
        if let Err(e) = self
            .registry
            .record_context_budget(&self.session_id, snapshot)
        {
            tracing::warn!(session_id = %self.session_id, "record_context_budget failed: {e}");
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
    /// publish the corresponding #2055 event kind, carrying the agent it was
    /// called with (UI Phase 1).
    #[tokio::test]
    async fn forwards_started_finished_and_error() {
        let registry = Arc::new(SessionRegistry::new());
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let sink = SessionToolEventSink::new(Arc::clone(&registry), session.id.clone());
        let mut events = crate::events::subscribe();

        sink.tool_started("pm", "call-1", "bash", "echo hi").await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "tool_started");
        assert!(
            matches!(ev.event, crate::events::Event::ToolStarted { agent, .. } if agent == "pm")
        );

        sink.tool_finished("pm", "call-1", "bash", true, "hi").await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "tool_finished");

        sink.tool_error("pm", "call-1", "bash", "boom").await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "tool_error");
    }

    /// ONE shared sink instance must attribute events to whichever agent
    /// calls it (UI Phase 1).
    ///
    /// Why: this is the whole reason `agent` is a per-call PARAMETER rather
    /// than sink state — `task::executor::run_and_record` clones ONE `Arc` of
    /// this sink into both the PM's loop and the delegated engineer's runner.
    /// A sink that carried its own agent name could only ever report one of
    /// them, leaving the two indistinguishable on the stream.
    #[tokio::test]
    async fn one_shared_sink_attributes_each_caller_separately() {
        let registry = Arc::new(SessionRegistry::new());
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let sink: Arc<dyn ToolEventSink> = Arc::new(SessionToolEventSink::new(
            Arc::clone(&registry),
            session.id.clone(),
        ));
        // Two clones of the ONE Arc, exactly as production wires it.
        let pm_view = Arc::clone(&sink);
        let engineer_view = Arc::clone(&sink);
        let mut events = crate::events::subscribe();

        pm_view
            .tool_started("pm", "c1", "delegate_to_agent", "{}")
            .await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert!(
            matches!(ev.event, crate::events::Event::ToolStarted { agent, .. } if agent == "pm")
        );

        engineer_view
            .tool_started("python-engineer", "c2", "bash", "cargo test")
            .await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert!(
            matches!(ev.event, crate::events::Event::ToolStarted { agent, .. }
                if agent == "python-engineer"),
            "a shared sink must report the CALLER's agent, not one fixed name"
        );
    }

    /// `tool_telemetry` must fan each variant out to its matching structured
    /// event (UI Phase 1).
    #[tokio::test]
    async fn forwards_search_and_recall_telemetry() {
        let registry = Arc::new(SessionRegistry::new());
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let sink = SessionToolEventSink::new(Arc::clone(&registry), session.id.clone());
        let mut events = crate::events::subscribe();

        sink.tool_telemetry(
            "python-engineer",
            "c1",
            "search_code",
            &ToolTelemetry::Search(crate::tools::SearchTelemetry {
                lane: "lexical".to_string(),
                query: "q".to_string(),
                hit_count: Some(2),
                latency_ms: 5,
            }),
        )
        .await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "search_performed");
        assert!(
            matches!(ev.event, crate::events::Event::SearchPerformed { agent, lane, .. }
                if agent == "python-engineer" && lane == "lexical")
        );

        sink.tool_telemetry(
            "pm",
            "c2",
            "recall_session",
            &ToolTelemetry::Recall(crate::tools::RecallTelemetry {
                query: "q".to_string(),
                results: vec![crate::events::RecalledMemory {
                    score: 0.4,
                    injected: false,
                }],
            }),
        )
        .await;
        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "memory_recalled");
        assert!(
            matches!(ev.event, crate::events::Event::MemoryRecalled { agent, .. } if agent == "pm")
        );
    }

    /// Hooks against an unknown session must not panic (they log and
    /// swallow the `session_not_found` error — the sink trait is `()`-only).
    #[tokio::test]
    async fn unknown_session_does_not_panic() {
        let registry = Arc::new(SessionRegistry::new());
        let sink = SessionToolEventSink::new(registry, "does-not-exist".to_string());
        sink.tool_started("pm", "c", "bash", "x").await;
        sink.tool_finished("pm", "c", "bash", true, "x").await;
        sink.tool_error("pm", "c", "bash", "x").await;
        sink.tool_telemetry(
            "pm",
            "c",
            "search_code",
            &ToolTelemetry::Search(crate::tools::SearchTelemetry {
                lane: "semantic".to_string(),
                query: "q".to_string(),
                hit_count: None,
                latency_ms: 1,
            }),
        )
        .await;
        sink.context_budget(&budget_snapshot()).await;
    }

    /// A representative budget snapshot for the sink tests.
    fn budget_snapshot() -> ContextBudgetSnapshot {
        ContextBudgetSnapshot {
            context_window: 200_000,
            overhead_tokens: 50_000,
            overhead_cap_tokens: 80_000,
            working_context_pct: 75,
            overhead_pct: 25,
            within_budget: true,
            fired: false,
            rounds: 1,
        }
    }

    /// The `context_budget` hook must reach the session's event stream as a
    /// `context_budget` event carrying the measured figures (epic #2343).
    ///
    /// Why: this is the last link in the chain that makes the Infinite
    /// Sessions guarantee renderable — `agent_loop` measures it, this sink is
    /// what binds it to a session and publishes it.
    #[tokio::test]
    async fn forwards_context_budget() {
        let registry = Arc::new(SessionRegistry::new());
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        let sink = SessionToolEventSink::new(Arc::clone(&registry), session.id.clone());
        let mut events = crate::events::subscribe();

        sink.context_budget(&budget_snapshot()).await;

        let ev = next_event_for(&mut events, &session.id).await;
        assert_eq!(ev.kind, "context_budget");
        assert!(matches!(
            ev.event,
            crate::events::Event::ContextBudget { working_context_pct, overhead_tokens, .. }
                if working_context_pct == 75 && overhead_tokens == 50_000
        ));
    }
}
