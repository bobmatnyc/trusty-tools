//! [`TmConnector`] — tm's [`WorkstreamConnector`] implementation (DOC-44
//! twin Phase 1, issue #3007).
//!
//! Why: DOC-44 §5.2 assigns tm's connector to wrap the daemon's existing
//! managed-session control surface. tm's daemon already exposes exactly the
//! six operations the trait needs as HTTP JSON routes under
//! `/api/v1/sessions/managed*` (`crates/trusty-mpm/src/daemon/api.rs:260-338`)
//! plus the loopback-only `POST /rpc` MCP bridge for `delegate` (no HTTP
//! route exists for `agent_delegate` — it is an MCP tool, reached the same
//! way the `serve --stdio` bridge reaches every other MCP tool). This module
//! is a thin translation layer: build the request, POST/GET the route,
//! translate the daemon's wire response into the trait's portable types.
//! What: [`TmConnector::new`] resolves the daemon URL via the existing
//! [`crate::core::discovery::resolve_daemon_url`] chain (default
//! `http://127.0.0.1:7880`); [`TmConnector::with_daemon_url`] targets an
//! explicit daemon (used by every test in `tm_tests.rs` to point at an
//! in-process test daemon on a random port). Every trait method maps 1:1 onto
//! a daemon route — see each method's own doc comment for the exact mapping.
//! Test: `tm_tests.rs` (a sibling `_tests.rs` file, exempt from the
//! production 500-SLOC cap per the workspace's test/bench file convention)
//! drives every method against a real in-process daemon router built with
//! `DaemonState::with_root_isolated_managed`, mirroring
//! `crates/trusty-mpm/src/client/proxy/tests.rs`'s `spawn_test_daemon`
//! pattern.

use async_trait::async_trait;
use serde_json::json;
use trusty_agents_common::connectors::{
    AgentSpec, AttachHandle, BackendParams, ConnectorError, CreateSessionReq, DelegateHandle,
    SessionInfo, SessionStatus, WorkstreamConnector,
};

use crate::client::{
    ManagedAttachCmdResponse, ManagedListResponse, ManagedSendInputResponse, ManagedSessionSummary,
    ManagedSpawnResponse, http_client::default_client,
};
use crate::core::discovery::resolve_daemon_url;

/// tm's [`WorkstreamConnector`] implementation over the daemon's HTTP API.
///
/// Why/What: see module docs.
pub struct TmConnector {
    http: reqwest::Client,
    daemon_url: String,
}

impl TmConnector {
    /// Build a connector targeting the daemon resolved via the standard
    /// discovery chain (explicit override env var, then lock file, then
    /// `http://127.0.0.1:7880`).
    ///
    /// Why: the common case — a caller (eventually the lead agent) that
    /// doesn't know or care which daemon address is in play.
    /// What: delegates to [`resolve_daemon_url`] with no explicit override.
    /// Test: `tm_tests::new_resolves_default_daemon_url`.
    pub fn new() -> Self {
        Self::with_daemon_url(resolve_daemon_url(None))
    }

    /// Build a connector targeting an explicit daemon URL.
    ///
    /// Why: tests (and any future multi-daemon caller) need to target a
    /// specific address rather than the discovery chain's default.
    /// What: stores `daemon_url` and a bounded-timeout `reqwest::Client`
    /// (the same [`default_client`] every other daemon-facing client in this
    /// crate uses).
    /// Test: every `tm_tests.rs` test builds one of these against an
    /// in-process test daemon's `http://127.0.0.1:<random port>`.
    pub fn with_daemon_url(daemon_url: impl Into<String>) -> Self {
        Self {
            http: default_client(),
            daemon_url: daemon_url.into(),
        }
    }
}

impl Default for TmConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a non-success HTTP response onto a [`ConnectorError`].
///
/// Why: shared by every method below so status-code-to-error-kind mapping
/// (404 -> `NotFound`, 400 -> `InvalidRequest`, everything else ->
/// `Backend`) stays in one place instead of drifting per call site.
/// What: reads the response body (best-effort — an unreadable body still
/// produces a message built from the status line alone) and classifies by
/// status code.
/// Test: `tm_tests::session_status_unknown_id_is_not_found` (404 path),
/// `tm_tests::create_session_wrong_backend_params_is_invalid_request`
/// (client-side 400 equivalent, though that specific test never reaches HTTP
/// — see its own docs).
async fn map_error_response(resp: reqwest::Response) -> ConnectorError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let message = if body.trim().is_empty() {
        status.to_string()
    } else {
        format!("{status}: {}", body.trim())
    };
    if status == reqwest::StatusCode::NOT_FOUND {
        ConnectorError::NotFound(message)
    } else if status == reqwest::StatusCode::BAD_REQUEST {
        ConnectorError::InvalidRequest(message)
    } else {
        ConnectorError::Backend(message)
    }
}

/// Map a `reqwest` transport-level failure (connection refused, timeout,
/// DNS) onto [`ConnectorError::Transport`].
fn transport_err(e: reqwest::Error) -> ConnectorError {
    ConnectorError::Transport(e.to_string())
}

/// Map a JSON-body-parse failure onto [`ConnectorError::Transport`] — the
/// daemon returned a success status but a response this connector could not
/// decode, which is a transport-shape problem, not a domain rejection.
fn decode_err(e: reqwest::Error) -> ConnectorError {
    ConnectorError::Transport(format!("failed to decode daemon response: {e}"))
}

#[async_trait]
impl WorkstreamConnector for TmConnector {
    /// `POST /api/v1/sessions/managed` — provision a new worktree session.
    ///
    /// Why: mirrors `daemon::managed_routes::spawn_session`'s request shape
    /// exactly (`repo_url`, `ref`, `task`, `name_hint`, `runtime`).
    /// `req.backend` must be [`BackendParams::Tm`] — a
    /// [`BackendParams::Tcode`] request is a caller bug (asked the tm
    /// connector to provision a tcode-shaped session) and is rejected with
    /// [`ConnectorError::InvalidRequest`] before any HTTP call is made.
    /// What: the request body is built by hand (rather than reusing
    /// [`crate::client::ManagedSpawnRequest`]) because that client DTO has
    /// no `ephemeral` field — this connector needs the full daemon
    /// `SpawnRequest` shape, including `ephemeral`, to satisfy
    /// [`BackendParams::Tm`]'s contract. The daemon's `#[serde(default)]`
    /// on every optional field means the extra key is harmless even against
    /// an older daemon. `req.name_hint`/`req.agent` map onto the common core
    /// fields; `agent` has no equivalent on tm's spawn request and is
    /// silently ignored (see [`CreateSessionReq`]'s docs on why that's
    /// intentional, not a data-loss bug).
    /// Test: `tm_tests::create_session_spawns_and_appears_in_list`,
    /// `tm_tests::create_session_wrong_backend_params_is_invalid_request`.
    async fn create_session(&self, req: CreateSessionReq) -> Result<SessionInfo, ConnectorError> {
        let CreateSessionReq {
            task,
            name_hint,
            agent: _agent,
            backend,
        } = req;
        let (repo_url, git_ref, runtime, ephemeral) = match backend {
            BackendParams::Tm {
                repo_url,
                git_ref,
                runtime,
                ephemeral,
            } => (repo_url, git_ref, runtime, ephemeral),
            BackendParams::Tcode { .. } => {
                return Err(ConnectorError::InvalidRequest(
                    "tm connector requires CreateSessionReq::backend = BackendParams::Tm".into(),
                ));
            }
        };

        let body = json!({
            "repo_url": repo_url,
            "ref": git_ref,
            "task": task,
            "name_hint": name_hint,
            "runtime": runtime,
            "ephemeral": ephemeral,
        });
        let url = format!("{}/api/v1/sessions/managed", self.daemon_url);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let spawned: ManagedSpawnResponse = resp.json().await.map_err(decode_err)?;
        Ok(SessionInfo {
            id: spawned.id,
            name: spawned.name,
            state: spawned.state,
            task: Some(task),
        })
    }

    /// `GET /api/v1/sessions/managed` — list every managed session.
    ///
    /// Why: mirrors `daemon::managed_routes::list_managed_sessions`.
    /// What: unwraps the `{"sessions": [...]}` envelope and maps each
    /// `ManagedSessionSummary` onto a portable `SessionInfo`.
    /// Test: `tm_tests::create_session_spawns_and_appears_in_list`,
    /// `tm_tests::list_sessions_empty_fleet_returns_empty_vec`.
    async fn list_sessions(&self) -> Result<Vec<SessionInfo>, ConnectorError> {
        let url = format!("{}/api/v1/sessions/managed", self.daemon_url);
        let resp = self.http.get(&url).send().await.map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let listed: ManagedListResponse = resp.json().await.map_err(decode_err)?;
        Ok(listed.sessions.into_iter().map(summary_to_info).collect())
    }

    /// `GET /api/v1/sessions/managed/{id}` — point lookup for one session.
    ///
    /// Why: mirrors `daemon::managed_routes::get_managed_session`.
    /// What: a 404 maps to [`ConnectorError::NotFound`] via
    /// [`map_error_response`]; otherwise the summary's `state` and
    /// `pending_decision` populate the [`SessionStatus`].
    /// Test: `tm_tests::session_status_returns_state_for_known_session`,
    /// `tm_tests::session_status_unknown_id_is_not_found`.
    async fn session_status(&self, session_id: &str) -> Result<SessionStatus, ConnectorError> {
        let url = format!("{}/api/v1/sessions/managed/{session_id}", self.daemon_url);
        let resp = self.http.get(&url).send().await.map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let summary: ManagedSessionSummary = resp.json().await.map_err(decode_err)?;
        Ok(SessionStatus {
            id: summary.id,
            state: summary.state,
            pending_decision: summary.pending_decision,
        })
    }

    /// `POST /api/v1/sessions/managed/{id}/send` — inject text into the pane.
    ///
    /// Why: mirrors `daemon::managed_routes::send_to_session`.
    /// What: a 404 (unknown session) maps to
    /// [`ConnectorError::NotFound`]; success is `()` — the daemon's
    /// `tmux_name` confirmation field is not surfaced through this trait.
    /// Test: `tm_tests::send_input_unknown_id_is_not_found`.
    async fn send_input(&self, session_id: &str, input: &str) -> Result<(), ConnectorError> {
        let url = format!(
            "{}/api/v1/sessions/managed/{session_id}/send",
            self.daemon_url
        );
        let resp = self
            .http
            .post(&url)
            .json(&json!({ "text": input }))
            .send()
            .await
            .map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let _ack: ManagedSendInputResponse = resp.json().await.map_err(decode_err)?;
        Ok(())
    }

    /// `GET /api/v1/sessions/managed/{id}/attach-cmd` — the tmux attach
    /// command.
    ///
    /// Why: mirrors `daemon::managed_routes::get_attach_cmd`. tm's attach
    /// surface is a shell command an operator runs directly — see
    /// [`AttachHandle::ShellCommand`]'s docs for why this is NOT forced into
    /// tcode's event-stream shape.
    /// What: wraps the daemon's `attach_cmd` string in
    /// [`AttachHandle::ShellCommand`]. A 404 (unknown session) maps to
    /// [`ConnectorError::NotFound`].
    /// Test: `tm_tests::attach_returns_shell_command_for_known_session`,
    /// `tm_tests::attach_unknown_id_is_not_found`.
    async fn attach(&self, session_id: &str) -> Result<AttachHandle, ConnectorError> {
        let url = format!(
            "{}/api/v1/sessions/managed/{session_id}/attach-cmd",
            self.daemon_url
        );
        let resp = self.http.get(&url).send().await.map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let cmd: ManagedAttachCmdResponse = resp.json().await.map_err(decode_err)?;
        Ok(AttachHandle::ShellCommand(cmd.attach_cmd))
    }

    /// Delegate work within a session — GATES and RECORDS only; does NOT
    /// execute the sub-agent (loud warning: read this before calling).
    ///
    /// **This method does not run any code.** It wraps tm's `agent_delegate`
    /// MCP tool (`crates/trusty-mpm/src/daemon/mcp_backend.rs`'s
    /// `agent_delegate`), which (1) consults the named agent's circuit
    /// breaker and refuses if it is open, then (2) RECORDS a `Delegation`
    /// entry for audit — it never spawns a process, never runs a subagent,
    /// and never touches the session's tmux pane. A caller that expects
    /// `delegate` to actually execute work will be surprised; that is tm's
    /// existing `agent_delegate` semantics, not a limitation this connector
    /// introduces. Real execution happens through
    /// [`WorkstreamConnector::send_input`] (injecting a delegation
    /// instruction into the session's own harness) or tm's separate
    /// spawn-a-sub-session paths — neither of which this trait method calls.
    ///
    /// Why: there is no dedicated HTTP route for `agent_delegate` — it is an
    /// MCP tool, reached via the loopback-only `POST /rpc` bridge
    /// (`crates/trusty-mpm/src/daemon/api/rpc.rs`) exactly as the `serve
    /// --stdio` proxy reaches every other MCP tool. This connector is the
    /// first HTTP-native (not MCP-stdio) caller of that bridge.
    /// What: POSTs a `tools/call` JSON-RPC envelope naming `agent_delegate`
    /// with `session_id`/`agent`/`task`/`tier` arguments. The MCP tool-call
    /// convention always returns HTTP 200 with the JSON-RPC `result` set (see
    /// `crate::mcp::dispatch_tool_call`'s docs) and puts BOTH success and
    /// failure into `result.content[0].text`, distinguished by
    /// `result.isError`; this method parses that convention and turns
    /// `isError: true` into [`ConnectorError::Backend`] (`"no such
    /// session"`/circuit-breaker-open messages land here) and a malformed
    /// envelope into [`ConnectorError::Transport`].
    /// Test: `tm_tests::delegate_records_a_delegation`,
    /// `tm_tests::delegate_unknown_session_is_backend_error`.
    async fn delegate(
        &self,
        session_id: &str,
        agent_spec: &AgentSpec,
    ) -> Result<DelegateHandle, ConnectorError> {
        let mut arguments = json!({
            "session_id": session_id,
            "agent": agent_spec.agent_name,
            "task": agent_spec.task,
        });
        if let Some(tier) = &agent_spec.tier {
            arguments["tier"] = json!(tier);
        }
        let rpc_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "agent_delegate", "arguments": arguments },
        });
        let url = format!("{}/rpc", self.daemon_url);
        let resp = self
            .http
            .post(&url)
            .json(&rpc_req)
            .send()
            .await
            .map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let envelope: serde_json::Value = resp.json().await.map_err(decode_err)?;
        let is_error = envelope
            .get("result")
            .and_then(|r| r.get("isError"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let text = envelope
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|block| block.get("text"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ConnectorError::Transport(format!(
                    "malformed agent_delegate tools/call response: {envelope}"
                ))
            })?;
        if is_error {
            return Err(ConnectorError::Backend(text.to_string()));
        }
        let parsed: serde_json::Value = serde_json::from_str(text).map_err(|e| {
            ConnectorError::Transport(format!("agent_delegate result was not JSON: {e}"))
        })?;
        let delegate_id = parsed
            .get("delegation_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ConnectorError::Transport("agent_delegate result missing delegation_id".to_string())
            })?
            .to_string();
        let note = parsed
            .get("circuit")
            .map(|v| format!("circuit breaker state: {v}"));
        Ok(DelegateHandle { delegate_id, note })
    }
}

/// Map a [`ManagedSessionSummary`] onto the portable [`SessionInfo`].
fn summary_to_info(summary: ManagedSessionSummary) -> SessionInfo {
    SessionInfo {
        id: summary.id,
        name: summary.name,
        state: summary.state,
        task: summary.task,
    }
}

#[cfg(test)]
#[path = "tm_tests.rs"]
mod tests;
