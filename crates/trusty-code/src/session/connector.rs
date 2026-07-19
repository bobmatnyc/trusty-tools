//! [`TcodeConnector`] — tcode's [`WorkstreamConnector`] implementation
//! (DOC-44 twin Phase 1, issue #3007).
//!
//! Why: DOC-44 §5.2 assigns tcode's connector to wrap the #2983 REST bridge
//! (`crate::serve::rest`) rather than reimplementing `session.*` business
//! logic — every REST route already just forwards to the identical
//! JSON-RPC method (`crate::session::protocol`) both `serve --stdio` and
//! `serve --http POST /rpc` dispatch through (see `crate::serve::rest`
//! module docs), so this connector calling REST is provably the SAME code
//! path a `POST /rpc` caller would hit. `attach` is the one exception —
//! there is no `GET`/`POST` REST route for `session.attach` (the vision spec
//! §4.4 SSE route, `GET /sessions/{id}/events`, is the real HTTP
//! live-streaming mechanism; `session.attach` itself only returns the
//! ring-buffer replay + that stream's URL) — so `attach` goes over
//! `POST /rpc` directly, exactly as `tests/session_e2e.rs`'s HTTP scenario
//! already does.
//! What: [`TcodeConnector::new`] targets the default `tcode serve --http`
//! port ([`crate::serve::DEFAULT_HTTP_PORT`], `7881`);
//! [`TcodeConnector::with_daemon_url`] targets an explicit daemon (every test
//! in `tests/connector_e2e.rs` uses this against a `--port 0` instance). Every
//! trait method's doc comment states the exact route/method it maps onto.
//! `delegate` is NOT implemented — see that method's docs (DOC-44 locked
//! decision 3).
//! Test: `tests/connector_e2e.rs` (a top-level integration test — trusty-code
//! has no `tests/support`-reachable unit-test seam for driving the REAL
//! `tcode` binary, so this mirrors `tests/session_e2e.rs`'s own placement)
//! drives every method against a real `tcode serve --http --port 0` process
//! via `support::spawn_http_daemon`.

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_agents_common::connectors::{
    AgentSpec, AttachHandle, BackendParams, ConnectorError, CreateSessionReq, DelegateHandle,
    SessionInfo, SessionStatus, WorkstreamConnector,
};

use super::model::Session;

/// tcode's [`WorkstreamConnector`] implementation over the #2983 REST bridge
/// (+ `POST /rpc` for `attach`).
///
/// Why/What: see module docs.
pub struct TcodeConnector {
    http: reqwest::Client,
    daemon_url: String,
}

impl TcodeConnector {
    /// Build a connector targeting `http://127.0.0.1:<DEFAULT_HTTP_PORT>`.
    ///
    /// Why: the common case for a caller that just wants "the local tcode
    /// daemon on its documented default port." Unlike tm, tcode has no
    /// lock-file/gateway discovery chain yet (DOC-44 §7.1 lists that as a
    /// tcode operational prerequisite, out of scope here) — this is a fixed
    /// default, matching how the CLI itself defaults `--port`.
    /// What: delegates to [`Self::with_daemon_url`] with
    /// `http://127.0.0.1:{DEFAULT_HTTP_PORT}`.
    /// Test: exercised indirectly — every `connector_e2e.rs` test builds via
    /// [`Self::with_daemon_url`] instead (an ephemeral `--port 0` instance),
    /// since asserting on this constructor's exact URL would require either
    /// binding the real default port (risking a collision with a developer's
    /// already-running `tcode serve`) or exposing `daemon_url` for
    /// inspection, which would leak an implementation detail no caller needs.
    pub fn new() -> Self {
        Self::with_daemon_url(format!(
            "http://127.0.0.1:{}",
            crate::serve::DEFAULT_HTTP_PORT
        ))
    }

    /// Build a connector targeting an explicit daemon URL.
    ///
    /// Why: tests need to target a `--port 0` ephemeral instance rather than
    /// the fixed default.
    /// What: stores `daemon_url` and a plain `reqwest::Client`.
    pub fn with_daemon_url(daemon_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            daemon_url: daemon_url.into(),
        }
    }
}

impl Default for TcodeConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a non-success HTTP response onto a [`ConnectorError`] (shared by the
/// REST-route methods below).
///
/// Why: `rpc_error_to_status` (`crate::serve::rest::rpc_error_to_status`)
/// already maps every domain error code onto the right HTTP status
/// (404/400/403/500) before this connector ever sees the response — so
/// classifying purely by status code, rather than re-parsing the JSON-RPC
/// error envelope in the body, is sufficient and keeps this connector
/// decoupled from that internal mapping's exact error-code table.
/// What: 404 -> `NotFound`, 400 -> `InvalidRequest`, else -> `Backend`.
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

/// Map a JSON-RPC error OBJECT (the `error` field of a `POST /rpc` envelope)
/// onto a [`ConnectorError`] — used only by `attach`, since `POST /rpc`
/// always returns HTTP 200 and carries its error in the envelope (unlike the
/// REST routes, which get a real HTTP status from `rpc_error_to_status`).
///
/// Why: mirrors `crate::serve::rest::rpc_error_to_status`'s code table so
/// `attach`'s error classification agrees with every REST-routed method's.
/// What: `-32007`/`-32002` -> `NotFound`; `-32003`/`-32602` ->
/// `InvalidRequest`; everything else -> `Backend`.
fn map_rpc_error_object(err: &Value) -> ConnectorError {
    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown JSON-RPC error")
        .to_string();
    match code {
        -32007 | -32002 => ConnectorError::NotFound(message),
        -32003 | -32602 => ConnectorError::InvalidRequest(message),
        _ => ConnectorError::Backend(message),
    }
}

/// Map a `reqwest` transport-level failure onto [`ConnectorError::Transport`].
fn transport_err(e: reqwest::Error) -> ConnectorError {
    ConnectorError::Transport(e.to_string())
}

/// Map a JSON-body-decode failure onto [`ConnectorError::Transport`].
fn decode_err(e: reqwest::Error) -> ConnectorError {
    ConnectorError::Transport(format!("failed to decode daemon response: {e}"))
}

/// Map a [`Session`] onto the portable [`SessionInfo`].
///
/// Why: tcode's `Session` has no separate display-name field (unlike tm's
/// tmux session name) — `name` mirrors `task` so callers always have SOME
/// human-readable label, documented here rather than silently duplicating
/// the field with no explanation.
fn session_to_info(session: Session) -> SessionInfo {
    SessionInfo {
        id: session.id,
        name: session.task.clone(),
        state: session.status.as_str().to_string(),
        task: Some(session.task),
    }
}

#[async_trait]
impl WorkstreamConnector for TcodeConnector {
    /// `POST /sessions` -> `session.create` (#2983 Slice 3).
    ///
    /// Why: mints a new tcode session bound to an existing local project.
    /// `req.backend` must be [`BackendParams::Tcode`] — a
    /// [`BackendParams::Tm`] request is a caller bug (asked the tcode
    /// connector to provision a tm-shaped worktree session) and is rejected
    /// with [`ConnectorError::InvalidRequest`] before any HTTP call is made.
    /// What: `req.name_hint` has no tcode equivalent and is silently
    /// ignored (see [`CreateSessionReq`]'s docs); `req.agent` maps onto
    /// `session.create`'s `agent` param. A `400` (empty task, or `project`
    /// naming no real directory — see `session::protocol::create`'s AC-16.2
    /// docs) maps to [`ConnectorError::InvalidRequest`].
    /// Test: `connector_e2e::create_session_full_lifecycle`,
    /// `connector_e2e::create_session_wrong_backend_params_is_invalid_request`.
    async fn create_session(&self, req: CreateSessionReq) -> Result<SessionInfo, ConnectorError> {
        let CreateSessionReq {
            task,
            name_hint: _name_hint,
            agent,
            backend,
        } = req;
        let project = match backend {
            BackendParams::Tcode { project } => project,
            BackendParams::Tm { .. } => {
                return Err(ConnectorError::InvalidRequest(
                    "tcode connector requires CreateSessionReq::backend = BackendParams::Tcode"
                        .into(),
                ));
            }
        };
        let body = json!({
            "task": task,
            "agent": agent,
            "project": project.to_string_lossy(),
        });
        let url = format!("{}/sessions", self.daemon_url);
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
        let session: Session = resp.json().await.map_err(decode_err)?;
        Ok(session_to_info(session))
    }

    /// `GET /sessions` -> `session.list` (#2983 Slice 2).
    ///
    /// Why: enumerates every session the daemon currently owns.
    /// What: unwraps the `{"sessions": [...]}` envelope and maps each
    /// `Session` onto a portable `SessionInfo`.
    /// Test: `connector_e2e::list_sessions_empty_fleet_returns_empty_vec`.
    async fn list_sessions(&self) -> Result<Vec<SessionInfo>, ConnectorError> {
        #[derive(serde::Deserialize)]
        struct SessionsEnvelope {
            sessions: Vec<Session>,
        }
        let url = format!("{}/sessions", self.daemon_url);
        let resp = self.http.get(&url).send().await.map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let listed: SessionsEnvelope = resp.json().await.map_err(decode_err)?;
        Ok(listed.sessions.into_iter().map(session_to_info).collect())
    }

    /// `GET /sessions/{id}` -> `session.status` (#2983 Slice 2).
    ///
    /// Why: point lookup for one session's current state.
    /// What: a `404` (unknown id, `-32007 session_not_found`) maps to
    /// [`ConnectorError::NotFound`]. `pending_decision` is always `None` —
    /// tcode has no equivalent concept in this phase (tm's managed-session
    /// pending-decision surface has no tcode counterpart yet).
    /// Test: `connector_e2e::session_status_unknown_id_is_not_found`.
    async fn session_status(&self, session_id: &str) -> Result<SessionStatus, ConnectorError> {
        let url = format!("{}/sessions/{session_id}", self.daemon_url);
        let resp = self.http.get(&url).send().await.map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        let session: Session = resp.json().await.map_err(decode_err)?;
        Ok(SessionStatus {
            id: session.id,
            state: session.status.as_str().to_string(),
            pending_decision: None,
        })
    }

    /// `POST /sessions/{id}/messages` -> `session.send` (#2983 Slice 3).
    ///
    /// Why: the client -> daemon input path.
    /// What: a `404` (unknown id) maps to [`ConnectorError::NotFound`];
    /// success is `()` — the daemon's `{"acknowledged": true}` confirmation
    /// is not surfaced through this trait.
    /// Test: `connector_e2e::send_input_unknown_id_is_not_found`.
    async fn send_input(&self, session_id: &str, input: &str) -> Result<(), ConnectorError> {
        let url = format!("{}/sessions/{session_id}/messages", self.daemon_url);
        let resp = self
            .http
            .post(&url)
            .json(&json!({ "input": input }))
            .send()
            .await
            .map_err(transport_err)?;
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        Ok(())
    }

    /// `POST /rpc` `session.attach` — ring-buffer replay + SSE stream URL.
    ///
    /// Why: there is no REST route for `session.attach` (see module docs) —
    /// this is the one method that must speak raw JSON-RPC. tcode's attach
    /// surface is fundamentally different from tm's: it hands back replayed
    /// history plus a URL a caller GETs (as SSE) for live events, rather
    /// than a command a human runs — see [`AttachHandle::EventStream`]'s
    /// docs for why this is NOT forced into tm's shell-command shape.
    /// What: `POST /rpc` always returns HTTP 200 (errors are carried IN the
    /// envelope, per `crate::serve::http::rpc_handler`'s docs) — a non-200
    /// here means transport failure, mapped via [`map_error_response`]; an
    /// `error` field in the envelope is mapped via
    /// [`map_rpc_error_object`] (`-32007 session_not_found` ->
    /// [`ConnectorError::NotFound`]). On success, wraps
    /// `result.{session_id,stream_url,events}` in
    /// [`AttachHandle::EventStream`].
    /// Test: `connector_e2e::create_session_full_lifecycle` (covers the
    /// `EventStream` shape on success), `connector_e2e::attach_unknown_id_is_not_found`.
    async fn attach(&self, session_id: &str) -> Result<AttachHandle, ConnectorError> {
        let rpc_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session.attach",
            "params": { "session_id": session_id },
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
        let envelope: Value = resp.json().await.map_err(decode_err)?;
        if let Some(err) = envelope.get("error") {
            return Err(map_rpc_error_object(err));
        }
        let result = envelope.get("result").cloned().unwrap_or(Value::Null);
        let session_id_out = result
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or(session_id)
            .to_string();
        let stream_url = result
            .get("stream_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let replayed_events = result
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(AttachHandle::EventStream {
            session_id: session_id_out,
            stream_url,
            replayed_events,
        })
    }

    /// Always [`ConnectorError::NotSupported`] — tcode has no delegate
    /// surface in this phase (DOC-44 locked decision 3).
    ///
    /// Why: tm's `delegate` wraps `agent_delegate`'s gate+record-only MCP
    /// tool; tcode has no equivalent endpoint, and adding one is explicit
    /// scope creep for issue #3007 (the issue's locked design calls this out
    /// by name). Returning a typed `NotSupported` — rather than a generic
    /// `Backend`/`Transport` failure, and rather than making an HTTP call
    /// that would 404/`METHOD_NOT_FOUND` against a route that will never
    /// exist — lets a caller (the future lead agent, or a test) distinguish
    /// "this backend never does this" from "this particular call failed."
    /// What: returns `Err(ConnectorError::NotSupported(_))` unconditionally,
    /// with no network call at all.
    /// Test: `connector_e2e::delegate_is_not_supported`.
    async fn delegate(
        &self,
        _session_id: &str,
        _agent_spec: &AgentSpec,
    ) -> Result<DelegateHandle, ConnectorError> {
        Err(ConnectorError::NotSupported(
            "tcode has no delegate surface in this phase (DOC-44 locked decision 3, issue #3007)"
                .into(),
        ))
    }
}
