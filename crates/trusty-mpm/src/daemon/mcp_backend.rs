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
use crate::session_manager::record::ManagedSessionId;
use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{session_record_kind::SessionRecordKind, state::DaemonState};

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
    /// Return every session — legacy AND managed — as a JSON array.
    ///
    /// Why: a session provisioned via the managed path (`session_new` /
    /// `spawn_managed` / the in-project worktree spawn — the tm day-to-day path)
    /// lives in the [`crate::session_manager::SessionManager`] store, NOT the
    /// legacy `DaemonState` registry. The sibling `session_stop` / `session_resume`
    /// tools already target that store by id, so listing only the legacy registry
    /// left provisioned sessions invisible: the operator could never discover the
    /// id needed to stop the session they are in (#1946).
    /// What: unions the legacy `DaemonState.sessions` (native-process discovery,
    /// hook auto-register, `POST /sessions` bookkeeping — tagged `kind: "legacy"`)
    /// with every `SessionManager` record (tagged `kind: "managed"`, serialized via
    /// the shared [`crate::daemon::managed_routes::record_to_json`] so callers get
    /// the managed id + `workspace_path`/`cwd` used to target `session_stop`).
    /// Test: `session_list_returns_registered_sessions` (legacy path) and
    /// `session_list_includes_managed_sessions` (#1946 regression) in `tests`.
    async fn session_list(&self) -> Result<Value, String> {
        let mut items: Vec<Value> = Vec::new();
        // Legacy in-memory registry: native-process discovery, hook
        // auto-registration, and `POST /sessions` bookkeeping.
        for session in &self.state.list_sessions() {
            let mut value = serde_json::to_value(session).map_err(|e| e.to_string())?;
            if let Value::Object(map) = &mut value {
                map.insert("kind".into(), SessionRecordKind::Legacy.as_str().into());
            }
            items.push(value);
        }
        // Managed store: the sessions `session_stop`/`session_resume` can target.
        let manager = self.state.session_manager().await;
        for record in manager.list().await {
            let mut value = crate::daemon::managed_routes::record_to_json(&record);
            if let Value::Object(map) = &mut value {
                map.insert("kind".into(), SessionRecordKind::Managed.as_str().into());
            }
            items.push(value);
        }
        Ok(Value::Array(items))
    }

    /// Return one session plus its memory snapshot and delegation count.
    ///
    /// Resolves BOTH session families (#1976): the legacy in-process
    /// `DaemonState` registry first, then — on a miss — the managed
    /// `SessionManager` store. Without the managed fallback, `session_status`
    /// reported "no such session" for the `tmpm-` sessions trusty-mpm itself
    /// spawns, even though `session_list` surfaces them. Managed records are
    /// serialized via the shared `record_to_json` (matching `session_list`) and
    /// carry no memory snapshot; delegations are keyed by UUID so they resolve
    /// for either family.
    async fn session_status(&self, session_id: &str) -> Result<Value, String> {
        let id = parse_session_id(session_id)?;
        // Legacy in-process registry first.
        if let Some(session) = self.state.session(id) {
            let memory = self.state.memory_for(id);
            let delegations = self.state.delegations_for(id);
            return Ok(json!({
                "session": session,
                "kind": SessionRecordKind::Legacy.as_str(),
                "memory": memory,
                "delegation_count": delegations.len(),
                "delegations": delegations,
            }));
        }
        // Managed store fallback (#1976): the `tmpm-` sessions `session_list`
        // surfaces and `session_stop`/`session_resume` already target by id.
        let manager = self.state.session_manager().await;
        if let Ok(record) = manager.get(&ManagedSessionId(id.0)).await {
            let delegations = self.state.delegations_for(id);
            return Ok(json!({
                "session": crate::daemon::managed_routes::record_to_json(&record),
                "kind": SessionRecordKind::Managed.as_str(),
                "memory": Value::Null,
                "delegation_count": delegations.len(),
                "delegations": delegations,
            }));
        }
        Err(format!("no such session: {session_id}"))
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
        // Accept BOTH session families (#1976): the gating registry historically
        // only knew the legacy in-process registry, so delegations from the
        // managed (`tmpm-`) sessions trusty-mpm itself spawns were rejected
        // outright. Fall back to the managed store on a legacy miss. The
        // delegation is keyed by UUID, so tracking works for either family.
        let known = self.state.session(id).is_some()
            || self
                .state
                .session_manager()
                .await
                .get(&ManagedSessionId(id.0))
                .await
                .is_ok();
        if !known {
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
    ///      and trusty-mpm JSONL stores so the MCP user sees a unified view. The
    ///      body lives in `super::mcp_bugreport` (a thin wrapper, matching the
    ///      `mcp_session`/`mcp_console`/`mcp_project` sibling-module convention)
    ///      to keep this file under the 500-SLOC cap.
    /// What: delegates to [`super::mcp_bugreport::list_recent_errors`].
    /// Test: `list_recent_errors_returns_valid_json` in the `tests` module.
    async fn list_recent_errors(&self, limit: u64) -> Result<Value, String> {
        super::mcp_bugreport::list_recent_errors(limit).await
    }

    /// Build and return the scrubbed issue preview for the given fingerprint.
    ///
    /// Why: the user must review the exact body that will be filed before
    ///      consenting. The preview IS the filed body — no transformation happens
    ///      between preview and filing.
    /// What: delegates to [`super::mcp_bugreport::preview_bug_report`].
    /// Test: `preview_bug_report_unknown_fingerprint_errors` in the `tests` module.
    async fn preview_bug_report(&self, fingerprint: &str) -> Result<Value, String> {
        super::mcp_bugreport::preview_bug_report(fingerprint).await
    }

    /// File or increment a GitHub issue for the given fingerprint.
    ///
    /// Why: the consent gate — nothing is filed unless `confirm` is `true`.
    /// What: delegates to [`super::mcp_bugreport::report_bug`], which resolves
    ///       the token, checks the rate-limit guard, and files via GitHub.
    /// Test: `report_bug_no_confirm_returns_preview_only`,
    ///       `report_bug_confirm_no_token_graceful_failure` in the `tests` module.
    async fn report_bug(&self, fingerprint: &str, confirm: bool) -> Result<Value, String> {
        super::mcp_bugreport::report_bug(fingerprint, confirm).await
    }

    // ── #1221: session-lifecycle tools ───────────────────────────────────────
    //
    // Each method is a thin wrapper over the existing `SessionManager` lifecycle
    // ops (the same ones the HTTP `…/managed/*` routes use), so MCP and HTTP
    // share one implementation. The wrapping logic lives in
    // `super::mcp_session` to keep this file under the 500-SLOC cap.

    async fn session_new(
        &self,
        repo_url: &str,
        git_ref: &str,
        task: &str,
        name_hint: Option<&str>,
        runtime: Option<&str>,
        ephemeral: Option<bool>,
    ) -> Result<Value, String> {
        super::mcp_session::session_new(
            &self.state,
            repo_url,
            git_ref,
            task,
            name_hint,
            runtime,
            ephemeral,
        )
        .await
    }

    async fn session_stop(&self, session_id: &str) -> Result<Value, String> {
        super::mcp_session::session_stop(&self.state, session_id).await
    }

    async fn session_resume(&self, session_id: &str) -> Result<Value, String> {
        super::mcp_session::session_resume(&self.state, session_id).await
    }

    async fn session_decommission(&self, session_id: &str) -> Result<Value, String> {
        super::mcp_session::session_decommission(&self.state, session_id).await
    }

    async fn session_delete(&self, session_id: &str, force: bool) -> Result<Value, String> {
        super::mcp_session::session_delete(&self.state, session_id, force).await
    }

    async fn session_activity(&self, session_id: &str, lines: u32) -> Result<Value, String> {
        super::mcp_session::session_activity(&self.state, session_id, lines).await
    }

    async fn session_send(&self, session_id: &str, text: &str) -> Result<Value, String> {
        super::mcp_session::session_send(&self.state, session_id, text).await
    }

    // ── PM pause/resume context tools (delegate to mcp_context) ──────────────

    async fn session_context_catchup(
        &self,
        project_dir: &str,
        session_id: Option<&str>,
        all_projects: bool,
        full: bool,
    ) -> Result<Value, String> {
        super::mcp_context::session_context_catchup(project_dir, session_id, all_projects, full)
            .await
    }

    async fn session_context_pause(
        &self,
        project_dir: &str,
        session_id: &str,
        summary: &str,
        completed: Vec<String>,
        in_progress: Vec<String>,
        next_steps: Vec<String>,
        tmux_window: Option<&str>,
        prune_worktrees: bool,
    ) -> Result<Value, String> {
        super::mcp_context::session_context_pause(
            &self.state,
            project_dir,
            session_id,
            summary,
            completed,
            in_progress,
            next_steps,
            tmux_window,
            prune_worktrees,
        )
        .await
    }

    // ── #1508: fleet-wide teardown tools (delegate to mcp_session) ───────────

    async fn session_decommission_ephemeral(&self) -> Result<Value, String> {
        super::mcp_session::session_decommission_ephemeral(&self.state).await
    }

    async fn session_prune(
        &self,
        state: &str,
        dry_run: bool,
        include_active: bool,
    ) -> Result<Value, String> {
        super::mcp_session::session_prune(&self.state, state, dry_run, include_active).await
    }

    // ── #1222: console-facing tools (delegate to mcp_console) ────────────────

    async fn console_metrics(&self) -> Result<Value, String> {
        super::mcp_console::console_metrics(&self.state).await
    }

    async fn supervisor_status(&self) -> Result<Value, String> {
        super::mcp_console::supervisor_status(&self.state).await
    }

    async fn auto_resume_set(&self, enabled: bool) -> Result<Value, String> {
        super::mcp_console::auto_resume_set(enabled).await
    }

    // ── #1220: config-convention tools (delegate to mcp_console) ─────────────

    async fn config_read(&self) -> Result<Value, String> {
        super::mcp_console::config_read()
    }

    #[allow(clippy::too_many_arguments)]
    async fn config_write(
        &self,
        workspace_root_template: Option<&str>,
        auto_resume: Option<bool>,
        default_model: Option<&str>,
        project_name: Option<&str>,
        github_config_dir: Option<&str>,
        github_token_env: Option<&str>,
        github_account: Option<&str>,
        github_host: Option<&str>,
        commit_name: Option<&str>,
        commit_email: Option<&str>,
        untracked_sync_patterns: Option<Vec<String>>,
        untracked_sync_enabled: Option<bool>,
    ) -> Result<Value, String> {
        super::mcp_console::config_write(
            workspace_root_template,
            auto_resume,
            default_model,
            project_name,
            github_config_dir,
            github_token_env,
            github_account,
            github_host,
            commit_name,
            commit_email,
            untracked_sync_patterns,
            untracked_sync_enabled,
        )
    }

    // ── #1519 WI-2: project-registry tools (delegate to mcp_project) ─────────

    async fn project_list(&self) -> Result<Value, String> {
        super::mcp_project::project_list(&self.state).await
    }

    async fn project_register(
        &self,
        name: &str,
        repo_url: &str,
        default_branch: Option<&str>,
        stack_hint: Option<&str>,
        tags: Option<Vec<String>>,
        description: Option<&str>,
        gh_user: Option<&str>,
        gh_account: Option<&str>,
    ) -> Result<Value, String> {
        super::mcp_project::project_register(
            &self.state,
            name,
            repo_url,
            default_branch,
            stack_hint,
            tags,
            description,
            gh_user,
            gh_account,
        )
        .await
    }

    async fn project_get(&self, name: &str) -> Result<Value, String> {
        super::mcp_project::project_get(&self.state, name).await
    }

    // ── #1517 WI-5: NL→repo resolver (delegates to mcp_project) ─────────────

    async fn project_resolve(&self, query: &str) -> Result<Value, String> {
        super::mcp_project::project_resolve(&self.state, query).await
    }

    // ── #2550: session-manager proxy tools (delegate to mcp_proxy) ───────────
    //
    // Each builds a `SessionProxy` over the SAME shared focus store the HTTP
    // proxy routes use, so a focus set over MCP is visible to those surfaces
    // under the same `conversation_key` and vice versa.

    async fn session_proxy_focus(
        &self,
        conversation_key: &str,
        session_id: &str,
    ) -> Result<Value, String> {
        super::mcp_proxy::session_proxy_focus(&self.state, conversation_key, session_id).await
    }

    async fn session_proxy_unfocus(&self, conversation_key: &str) -> Result<Value, String> {
        super::mcp_proxy::session_proxy_unfocus(&self.state, conversation_key).await
    }

    async fn session_proxy_message(
        &self,
        conversation_key: &str,
        text: &str,
    ) -> Result<Value, String> {
        super::mcp_proxy::session_proxy_message(&self.state, conversation_key, text).await
    }

    async fn session_proxy_summary(&self, conversation_key: &str) -> Result<Value, String> {
        super::mcp_proxy::session_proxy_summary(&self.state, conversation_key).await
    }
}

#[cfg(test)]
#[path = "mcp_backend_tests.rs"]
mod tests;
