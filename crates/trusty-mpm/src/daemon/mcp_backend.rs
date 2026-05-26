//! Daemon-side implementation of the MCP orchestration backend.
//!
//! Why: `trusty-mpm-mcp` defines the `OrchestratorBackend` trait but no
//! behaviour — the protocol crate is deliberately ignorant of daemon state.
//! This module is the Anti-Corruption Layer that translates MCP tool calls
//! into mutations on [`DaemonState`], so Claude Code sessions can drive the
//! orchestrator without reaching into its internals.
//! What: [`StateBackend`] wraps `Arc<DaemonState>` and implements every
//! `OrchestratorBackend` method by reading/writing the shared state.
//! Test: `cargo test -p trusty-mpm-daemon` calls each backend method against a
//! freshly-built state and asserts the JSON results.

use std::sync::Arc;

use crate::core::agent::{Delegation, ModelTier};
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::memory::MemoryUsage;
use crate::core::session::SessionId;
use crate::mcp::OrchestratorBackend;
use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use super::state::DaemonState;

/// MCP backend backed by the daemon's shared state.
///
/// Why: a thin adapter keeps the protocol crate and the state crate decoupled
/// — either can be tested without the other.
/// What: holds an `Arc<DaemonState>` clone; cheap to construct per connection.
/// Test: see the module tests.
#[derive(Clone)]
pub struct StateBackend {
    state: Arc<DaemonState>,
}

impl StateBackend {
    /// Build a backend over shared daemon state.
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

/// Parse a session-id string into a `SessionId`, mapping failure to a message.
fn parse_session_id(raw: &str) -> Result<SessionId, String> {
    Uuid::parse_str(raw)
        .map(SessionId)
        .map_err(|_| format!("`{raw}` is not a valid session id (expected a UUID)"))
}

#[async_trait]
impl OrchestratorBackend for StateBackend {
    /// Return every managed session as a JSON array.
    async fn session_list(&self) -> Result<Value, String> {
        let sessions = self.state.list_sessions();
        serde_json::to_value(&sessions).map_err(|e| e.to_string())
    }

    /// Return one session plus its memory snapshot and delegation count.
    async fn session_status(&self, session_id: &str) -> Result<Value, String> {
        let id = parse_session_id(session_id)?;
        let session = self
            .state
            .session(id)
            .ok_or_else(|| format!("no such session: {session_id}"))?;
        let memory = self.state.memory_for(id);
        let delegations = self.state.delegations_for(id);
        Ok(json!({
            "session": session,
            "memory": memory,
            "delegation_count": delegations.len(),
            "delegations": delegations,
        }))
    }

    /// Gate and record a new agent delegation.
    ///
    /// The circuit breaker is consulted first: an open breaker refuses the
    /// delegation with an explanatory error instead of silently queueing it.
    async fn agent_delegate(
        &self,
        session_id: &str,
        agent: &str,
        task: &str,
        tier: Option<&str>,
    ) -> Result<Value, String> {
        let id = parse_session_id(session_id)?;
        if self.state.session(id).is_none() {
            return Err(format!("no such session: {session_id}"));
        }
        let breaker = self.state.breaker(agent);
        if !breaker.allows_delegation() {
            return Err(format!(
                "circuit breaker for agent `{agent}` is {:?}; delegation refused",
                breaker.state
            ));
        }
        let tier = match tier {
            Some("haiku") => ModelTier::Haiku,
            Some("sonnet") => ModelTier::Sonnet,
            Some("opus") => ModelTier::Opus,
            Some(other) => return Err(format!("unknown model tier: `{other}`")),
            None => ModelTier::Sonnet,
        };
        let delegation = Delegation::new(id, None, agent, tier, task);
        let delegation_id = delegation.id;
        self.state.upsert_delegation(delegation);
        Ok(json!({
            "delegation_id": delegation_id.0,
            "agent": agent,
            "tier": tier,
            "circuit": breaker.state,
        }))
    }

    /// Record token usage and report the resulting memory pressure.
    async fn memory_protect(
        &self,
        session_id: &str,
        used_tokens: u64,
        window_tokens: u64,
    ) -> Result<Value, String> {
        let id = parse_session_id(session_id)?;
        if window_tokens == 0 {
            return Err("window_tokens must be greater than zero".into());
        }
        let usage = MemoryUsage {
            used_tokens,
            window_tokens,
        };
        let pressure = self.state.record_memory(id, usage);
        Ok(json!({
            "fraction": usage.fraction(),
            "pressure": pressure,
            "config": self.state.memory_config,
        }))
    }

    /// Return one or all agents' circuit-breaker states.
    async fn circuit_breaker_status(&self, agent: Option<&str>) -> Result<Value, String> {
        match agent {
            Some(name) => {
                let cb = self.state.breaker(name);
                Ok(json!({ "agent": name, "breaker": cb }))
            }
            None => {
                let all: Vec<Value> = self
                    .state
                    .all_breakers()
                    .into_iter()
                    .map(|(name, cb)| json!({ "agent": name, "breaker": cb }))
                    .collect();
                Ok(json!({ "breakers": all }))
            }
        }
    }

    /// Ingest a Claude Code hook event into the observability ring buffer.
    ///
    /// Subagent-stop events additionally feed the agent's circuit breaker so
    /// repeated failures trip it: a `SubagentStopFailure` is a failure, a plain
    /// `SubagentStop` a success. The agent name is read from the payload's
    /// `agent` field when present.
    async fn hook_event(
        &self,
        session_id: &str,
        event: &str,
        payload: Value,
    ) -> Result<Value, String> {
        let id = parse_session_id(session_id)?;
        let parsed =
            HookEvent::from_wire(event).ok_or_else(|| format!("unknown hook event: `{event}`"))?;

        // Drive the circuit breaker from subagent lifecycle events.
        if let Some(agent) = payload.get("agent").and_then(Value::as_str) {
            match parsed {
                HookEvent::SubagentStop => self.state.record_outcome(agent, true),
                HookEvent::SubagentStopFailure => self.state.record_outcome(agent, false),
                _ => {}
            }
        }

        // Compress PostToolUse output before it enters the ring buffer.
        let mut payload = payload;
        if parsed == HookEvent::PostToolUse {
            let tool_name = payload
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let cfg = self.state.optimizer_config();
            super::optimizer::optimize_tool_output(&cfg, &tool_name, &mut payload);
        }

        self.state
            .push_hook_event(HookEventRecord::now(id, parsed, payload));
        Ok(json!({ "received": event, "session_id": session_id }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session::{ControlModel, Session, SessionStatus};

    fn state_with_session() -> (Arc<DaemonState>, SessionId) {
        let state = DaemonState::shared();
        let id = SessionId::new();
        let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        session.status = SessionStatus::Active;
        state.register_session(session);
        (state, id)
    }

    #[tokio::test]
    async fn session_list_returns_registered_sessions() {
        let (state, _) = state_with_session();
        let backend = StateBackend::new(state);
        let list = backend.session_list().await.unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn session_status_unknown_id_errors() {
        let (state, _) = state_with_session();
        let backend = StateBackend::new(state);
        let err = backend.session_status("not-a-uuid").await.unwrap_err();
        assert!(err.contains("not a valid session id"));
    }

    #[tokio::test]
    async fn agent_delegate_records_a_delegation() {
        let (state, id) = state_with_session();
        let backend = StateBackend::new(state.clone());
        let result = backend
            .agent_delegate(&id.0.to_string(), "research", "find the bug", Some("opus"))
            .await
            .unwrap();
        assert_eq!(result["agent"], "research");
        assert_eq!(state.delegations_for(id).len(), 1);
    }

    #[tokio::test]
    async fn agent_delegate_refused_when_breaker_open() {
        let (state, id) = state_with_session();
        // Trip the breaker for `flaky` with three failures.
        for _ in 0..3 {
            state.record_outcome("flaky", false);
        }
        let backend = StateBackend::new(state);
        let err = backend
            .agent_delegate(&id.0.to_string(), "flaky", "task", None)
            .await
            .unwrap_err();
        assert!(err.contains("circuit breaker"));
    }

    #[tokio::test]
    async fn memory_protect_classifies_pressure() {
        let (state, id) = state_with_session();
        let backend = StateBackend::new(state);
        let result = backend
            .memory_protect(&id.0.to_string(), 900, 1000)
            .await
            .unwrap();
        assert_eq!(result["pressure"], "Compact");
    }

    #[tokio::test]
    async fn hook_event_rejects_unknown_event() {
        let (state, id) = state_with_session();
        let backend = StateBackend::new(state);
        let err = backend
            .hook_event(&id.0.to_string(), "NotAnEvent", Value::Null)
            .await
            .unwrap_err();
        assert!(err.contains("unknown hook event"));
    }

    #[tokio::test]
    async fn hook_event_drives_circuit_breaker() {
        let (state, id) = state_with_session();
        let backend = StateBackend::new(state.clone());
        // Three subagent failures for `flaky` should trip its breaker.
        for _ in 0..3 {
            backend
                .hook_event(
                    &id.0.to_string(),
                    "SubagentStopFailure",
                    json!({ "agent": "flaky" }),
                )
                .await
                .unwrap();
        }
        assert!(!state.breaker("flaky").allows_delegation());
    }

    #[tokio::test]
    async fn hook_event_accepts_known_event() {
        let (state, id) = state_with_session();
        let backend = StateBackend::new(state.clone());
        backend
            .hook_event(&id.0.to_string(), "PreToolUse", json!({"tool": "Bash"}))
            .await
            .unwrap();
        assert_eq!(state.recent_hook_events().len(), 1);
    }
}
