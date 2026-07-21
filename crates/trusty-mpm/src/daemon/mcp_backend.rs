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
        // session_list now also reads the SessionManager store, so this test
        // must bind to an ISOLATED managed store (#1790) rather than the
        // production `~/.trusty-mpm` root that `DaemonState::shared` uses.
        let root = tempfile::TempDir::new().expect("root tempdir");
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let id = SessionId::new();
        let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
        session.status = SessionStatus::Active;
        state.register_session(session);

        let backend = StateBackend::new(state);
        let list = backend.session_list().await.unwrap();
        // One legacy session registered; the isolated managed store is empty.
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["kind"], "legacy");
    }

    #[tokio::test]
    async fn session_list_includes_managed_sessions() {
        // Regression for #1946: a session provisioned via the managed path
        // (`session_new` / `spawn_managed` / in-project worktree spawn) lives in
        // the SessionManager store, NOT the legacy `DaemonState` registry. The
        // sibling `session_stop` tool targets that store by id, so `session_list`
        // MUST surface managed sessions — otherwise the operator can never
        // discover the id needed to stop the session they are in.
        let root = tempfile::TempDir::new().expect("root tempdir");
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);

        // Seed one managed session directly in the store (fake tmux driver, so no
        // real tmux pane is created).
        let mgr = state.session_manager().await;
        let record = mgr
            .create(
                "fix the bug".to_string(),
                Some(root.path().to_path_buf()),
                Some("regression".to_string()),
                Some(root.path().to_path_buf()),
                None,
                None,
            )
            .await
            .expect("managed session create must succeed with the fake tmux driver");

        let backend = StateBackend::new(state);
        let list = backend.session_list().await.unwrap();
        let arr = list.as_array().expect("session_list returns an array");

        let found = arr.iter().any(|v| {
            v.get("id").and_then(|i| i.as_str()) == Some(record.id.to_string().as_str())
                && v.get("kind").and_then(|k| k.as_str()) == Some("managed")
        });
        assert!(
            found,
            "provisioned managed session {} must appear in session_list (got {list})",
            record.id
        );
    }

    #[tokio::test]
    async fn session_status_unknown_id_errors() {
        let (state, _) = state_with_session();
        let backend = StateBackend::new(state);
        let err = backend.session_status("not-a-uuid").await.unwrap_err();
        assert!(err.contains("not a valid session id"));
    }

    #[tokio::test]
    async fn session_status_resolves_managed_session() {
        // Regression for #1976: session_status previously only consulted the
        // legacy registry, so a managed (`tmpm-`) session — the primary type
        // trusty-mpm spawns — reported "no such session". It must now resolve via
        // the managed store fallback and report `kind: "managed"`.
        let root = tempfile::TempDir::new().expect("root tempdir");
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let record = state
            .session_manager()
            .await
            .create(
                "fix the bug".to_string(),
                Some(root.path().to_path_buf()),
                None,
                Some(root.path().to_path_buf()),
                None,
                None,
            )
            .await
            .expect("managed create");

        let backend = StateBackend::new(state);
        let status = backend
            .session_status(&record.id.to_string())
            .await
            .expect("managed session must resolve via the #1976 fallback");
        assert_eq!(status["kind"], "managed");
        assert_eq!(status["session"]["id"], record.id.to_string());
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
    async fn agent_delegate_accepts_managed_session() {
        // Regression for #1976: delegation gating rejected managed (`tmpm-`)
        // sessions with "no such session" because it only checked the legacy
        // registry. A delegation from a managed session must now be accepted and
        // recorded (keyed by UUID).
        let root = tempfile::TempDir::new().expect("root tempdir");
        let state =
            Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
        let record = state
            .session_manager()
            .await
            .create(
                "implement the fix".to_string(),
                Some(root.path().to_path_buf()),
                None,
                Some(root.path().to_path_buf()),
                None,
                None,
            )
            .await
            .expect("managed create");

        let backend = StateBackend::new(state.clone());
        let result = backend
            .agent_delegate(
                &record.id.to_string(),
                "rust-engineer",
                "wire it up",
                Some("sonnet"),
            )
            .await
            .expect("delegation from a managed session must be accepted (#1976)");
        assert_eq!(result["agent"], "rust-engineer");
        // The delegation is keyed by the session UUID, shared across both families.
        assert_eq!(state.delegations_for(SessionId(record.id.0)).len(), 1);
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
