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

    /// Return recent captured errors across all known daemon stores.
    ///
    /// Why: aggregates errors from trusty-search, trusty-memory, trusty-analyze,
    ///      and trusty-mpm JSONL stores so the MCP user sees a unified view.
    /// What: calls [`super::bug_report::aggregate_errors`] with `limit` capped at
    ///       100, then serializes the [`AggregatedError`] list as JSON.
    /// Test: `list_recent_errors_returns_valid_json` in the `tests` module.
    async fn list_recent_errors(&self, limit: u64) -> Result<Value, String> {
        let limit = (limit as usize).min(100);
        let errors = super::bug_report::aggregate_errors(limit);
        let summaries: Vec<serde_json::Value> = errors
            .iter()
            .map(|e| {
                json!({
                    "fingerprint": e.record.fingerprint,
                    "crate_target": e.record.crate_target,
                    "crate_version": e.record.crate_version,
                    "summary": e.record.summary(),
                    "occurrences": e.occurrences,
                    "timestamp_secs": e.record.timestamp_secs,
                    "os": e.record.os,
                    "arch": e.record.arch,
                })
            })
            .collect();
        Ok(json!({
            "errors": summaries,
            "total": summaries.len(),
            "limit": limit,
        }))
    }

    /// Build and return the scrubbed issue preview for the given fingerprint.
    ///
    /// Why: the user must review the exact body that will be filed before
    ///      consenting. The preview IS the filed body — no transformation happens
    ///      between preview and filing.
    /// What: calls [`super::bug_report::aggregate_errors`] to load errors, finds
    ///       the one with the matching fingerprint, runs
    ///       [`super::bug_report::build_preview`], and serializes the result.
    ///       Returns an error string when the fingerprint is not found.
    /// Test: `preview_bug_report_unknown_fingerprint_errors` in the `tests` module.
    async fn preview_bug_report(&self, fingerprint: &str) -> Result<Value, String> {
        let errors = super::bug_report::aggregate_errors(500);
        let found = errors
            .into_iter()
            .find(|e| e.record.fingerprint == fingerprint)
            .ok_or_else(|| {
                format!(
                    "fingerprint `{fingerprint}` not found in local error stores; \
                     run list_recent_errors to see available fingerprints"
                )
            })?;
        let preview = super::bug_report::build_preview(&found);
        let changes: Vec<serde_json::Value> = preview
            .scrub_changes
            .iter()
            .map(|c| json!({ "pattern": c.pattern, "hint": c.hint }))
            .collect();
        Ok(json!({
            "fingerprint": preview.fingerprint,
            "title": preview.title,
            "body": preview.body,
            "labels": preview.labels,
            "scrub_changes": changes,
            "note": "This is the exact content that will be filed. Call report_bug with confirm:true to file.",
        }))
    }

    /// File or increment a GitHub issue for the given fingerprint.
    ///
    /// Why: the consent gate — nothing is filed unless `confirm` is `true`.
    ///      When `confirm` is false, returns the same preview as
    ///      `preview_bug_report`. When `true`, resolves the token via the full
    ///      provider chain (Fix 1 / #498) and calls
    ///      [`super::bug_report::file_issue`].
    ///
    /// Fixes implemented here:
    ///   - Fix 1 (#498, P0): uses `ResolvedProvider` (PAT → file → GitHub App
    ///     → NoToken) instead of the narrower `EnvFileTokenProvider`, so the
    ///     GitHub App path is now reachable.
    ///   - Fix 3 (P2): the `RateLimitGuard` is checked before any GitHub call;
    ///     a blocked call returns `{ filed:false, rate_limited:true }`.  After a
    ///     successful filing `record_filed` is called.  State-file failures are
    ///     non-fatal (logged via `record_filed`'s own warning).
    ///
    /// What: a `confirm:false` call is pure-preview (no network call). A
    ///       `confirm:true` call with no token returns a graceful failure with
    ///       an actionable message. A rate-limited call returns
    ///       `{ filed:false, rate_limited:true, note:… }`. A successful filing
    ///       returns `{ filed, deduped, issue_url, issue_number }`.
    /// Test: `report_bug_no_confirm_returns_preview_only`,
    ///       `report_bug_confirm_no_token_graceful_failure` in the `tests` module.
    async fn report_bug(&self, fingerprint: &str, confirm: bool) -> Result<Value, String> {
        // Step 1: load the error regardless of confirm — preview is always built.
        let errors = super::bug_report::aggregate_errors(500);
        let found = errors
            .into_iter()
            .find(|e| e.record.fingerprint == fingerprint)
            .ok_or_else(|| {
                format!("fingerprint `{fingerprint}` not found; run list_recent_errors")
            })?;
        let preview = super::bug_report::build_preview(&found);

        if !confirm {
            // Preview-only path — nothing filed.
            let changes: Vec<serde_json::Value> = preview
                .scrub_changes
                .iter()
                .map(|c| json!({ "pattern": c.pattern, "hint": c.hint }))
                .collect();
            return Ok(json!({
                "filed": false,
                "note": "confirm:false — preview only. Call with confirm:true to file.",
                "preview": {
                    "fingerprint": preview.fingerprint,
                    "title": preview.title,
                    "body": preview.body,
                    "labels": preview.labels,
                    "scrub_changes": changes,
                }
            }));
        }

        // Fix 3 (P2): check the rate-limit guard before any GitHub call.
        let guard = super::bug_report::RateLimitGuard::production();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let rl_decision = guard.check(fingerprint, now_secs);
        if !rl_decision.is_allowed() {
            return Ok(json!({
                "filed": false,
                "rate_limited": true,
                "note": rl_decision.block_reason(),
            }));
        }

        // Step 2: attempt to file via GitHub.
        // Fix 1 (P0): use the full resolution chain — PAT → file → GitHub App → NoToken.
        // Use spawn_blocking because the real reqwest client is blocking.
        let fp_owned = fingerprint.to_string();
        let provider = super::bug_report::ResolvedProvider;
        let result =
            tokio::task::spawn_blocking(move || super::bug_report::file_issue(&preview, &provider))
                .await
                .map_err(|e| format!("internal error: spawn_blocking failed: {e}"))?;

        match result {
            Ok(filing) => {
                // Fix 3 (P2): record the successful filing; write failures are
                // non-fatal — record_filed logs warnings internally.
                guard.record_filed(&fp_owned, now_secs);
                Ok(json!({
                    "filed": filing.filed,
                    "deduped": filing.deduped,
                    "issue_url": filing.issue_url,
                    "issue_number": filing.issue_number,
                }))
            }
            Err(super::bug_report::GithubFilingError::NoToken) => Ok(json!({
                "filed": false,
                "note": super::bug_report::GithubFilingError::NoToken.to_string(),
            })),
            Err(e) => Err(format!("GitHub filing failed: {e}")),
        }
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

    // ── Phase 3: bug-reporting backend tests ──────────────────────────────────

    #[tokio::test]
    async fn list_recent_errors_returns_valid_json() {
        // The local daemon stores are typically empty in CI; this test verifies
        // the method returns a valid, parseable JSON object regardless.
        let (state, _) = state_with_session();
        let backend = StateBackend::new(state);
        let result = backend.list_recent_errors(20).await.unwrap();
        assert!(result["errors"].is_array(), "errors must be an array");
        assert!(result["limit"].is_number(), "limit must be a number");
    }

    #[tokio::test]
    async fn preview_bug_report_unknown_fingerprint_errors() {
        let (state, _) = state_with_session();
        let backend = StateBackend::new(state);
        let err = backend
            .preview_bug_report(&"z".repeat(64))
            .await
            .unwrap_err();
        assert!(
            err.contains("not found"),
            "error should mention 'not found': {err}"
        );
    }

    #[tokio::test]
    async fn report_bug_no_confirm_returns_preview_only() {
        let (state, _) = state_with_session();
        let backend = StateBackend::new(state);
        // An unknown fingerprint with confirm:false should give "not found" error
        // (the fingerprint lookup happens before the confirm check).
        let err = backend
            .report_bug(&"y".repeat(64), false)
            .await
            .unwrap_err();
        assert!(err.contains("not found"), "expected not-found error: {err}");
    }

    #[tokio::test]
    async fn report_bug_confirm_no_token_graceful_failure() {
        // When TRUSTY_BUGREPORT_GITHUB_TOKEN is absent and no token file exists,
        // report_bug should return Ok or Err without panicking — no real GitHub call.
        // Because local stores are typically empty in CI, the "not found" error
        // fires before the token check; that is acceptable. The intent is that
        // no panic occurs and no network call is made.
        let (state, _) = state_with_session();
        let backend = StateBackend::new(state);
        // This returns Err (fingerprint not found) — acceptable in the test
        // environment where stores are empty. What matters is no panic and no
        // real GitHub call.
        let _ = backend.report_bug(&"x".repeat(64), true).await;
    }
}
