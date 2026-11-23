//! The client boundary a handoff dispatches through (#11650, slice 3c).
//!
//! Why: tests must never reach a real tm daemon or tcode, so the two targets
//! sit behind small traits; the flow in `super` only sees [`TmClient`] and
//! [`TcClient`].
//! What:
//! - [`CallError`] — every failure says whether anything could have reached
//!   the target ([`CallError::Unknown`]) or not ([`CallError::NotSent`]).
//! - [`StdioTm`] — trusty-mpm through `tm serve --stdio`, the MCP bridge the
//!   `dispatch_task` tool already drives with [`StdioMcpClient`]
//!   (`session_new`, `session_activity`, `session_send`).
//! - [`UdsTc`] — trusty-code's `task.run` on the running `tcode serve`
//!   daemon's socket, with trusty-code's own [`UdsRpcClient`]. No daemon is
//!   started here; a missing daemon is a `NotSent` failure.
//!
//! Test: `super::tests::client_tests` for the result decoding; the flow
//! through fakes in `super::tests::flow_tests`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use trusty_code::tui_client::EngineError;
use trusty_code::tui_client::uds_rpc::UdsRpcClient;
use trusty_common::uds::rpc::UdsRpcError;

use crate::plugins::stdio_mcp::StdioMcpClient;

/// A failed call to a handoff target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// Nothing reached the target, or the target refused the request.
    NotSent(String),
    /// The request may have reached the target; its effect is not known.
    Unknown(String),
}

/// What tm reports about a recorded durable session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmSessionState {
    /// Provisioning or active: the task can be sent into it.
    Live,
    /// The record is gone, decommissioned or deleted. It cannot come back.
    Gone { reason: String },
    /// Stopped or errored: the record still exists and is resumable.
    Parked { state: String },
}

/// The tm (trusty-mpm) operations a handoff needs.
#[async_trait]
pub trait TmClient: Send + Sync {
    /// Create a durable managed session in `repo` that starts on `task`;
    /// returns its managed session id.
    async fn session_new(&self, repo: &Path, task: &str) -> Result<String, CallError>;
    /// Read a recorded session's state. Read-only.
    async fn session_state(&self, id: &str) -> Result<TmSessionState, CallError>;
    /// Send `text` into session `id`, submitted with Enter.
    async fn session_send(&self, id: &str, text: &str) -> Result<(), CallError>;
}

/// The tc (trusty-code) operation a handoff needs.
#[async_trait]
pub trait TcClient: Send + Sync {
    /// Call `task.run`; returns the `session_id` it answers with.
    async fn task_run(&self, task: &str, project: Option<&Path>) -> Result<String, CallError>;
}

/// trusty-mpm over `tm serve --stdio`, spawned on first use.
pub struct StdioTm {
    program: String,
    client: Mutex<Option<StdioMcpClient>>,
}

impl StdioTm {
    /// A client that runs `program serve --stdio` (normally `tm`) when first
    /// called. Construction spawns nothing.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            client: Mutex::new(None),
        }
    }

    /// Call one tm tool. A failure before the request is written is
    /// `NotSent`; a JSON-RPC error answer is `NotSent`; anything else after
    /// the write (timeout, EOF) is `Unknown`.
    async fn call(&self, tool: &str, args: Value) -> Result<ToolAnswer, CallError> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            let mut client =
                StdioMcpClient::spawn(&self.program, &["serve", "--stdio"], env!("CARGO_PKG_NAME"))
                    .await
                    .map_err(|e| {
                        CallError::NotSent(format!(
                            "cannot start `{} serve --stdio`: {e:#}",
                            self.program
                        ))
                    })?;
            client
                .initialize()
                .await
                .map_err(|e| CallError::NotSent(format!("tm MCP handshake failed: {e:#}")))?;
            *guard = Some(client);
        }
        let Some(client) = guard.as_mut() else {
            return Err(CallError::NotSent("tm client unavailable".into()));
        };
        match client.call_tool(tool, args).await {
            Ok(result) => Ok(ToolAnswer::from_result(&result)),
            // `extract_result` renders a JSON-RPC error object this way.
            Err(e) if e.to_string().starts_with("JSON-RPC error") => {
                Err(CallError::NotSent(format!("tm {tool}: {e:#}")))
            }
            Err(e) => Err(CallError::Unknown(format!("tm {tool}: {e:#}"))),
        }
    }
}

#[async_trait]
impl TmClient for StdioTm {
    async fn session_new(&self, repo: &Path, task: &str) -> Result<String, CallError> {
        let args = json!({
            "repo_url": repo.display().to_string(),
            "ref": "HEAD",
            "task": task,
            "ephemeral": false,
        });
        match self.call("session_new", args).await? {
            ToolAnswer::Error(text) => Err(CallError::NotSent(format!("tm session_new: {text}"))),
            ToolAnswer::Json(v) => v
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or(CallError::Unknown(
                    "tm session_new answered without a session id".into(),
                )),
        }
    }

    async fn session_state(&self, id: &str) -> Result<TmSessionState, CallError> {
        let args = json!({ "session_id": id, "lines": 1 });
        let answer = match self.call("session_activity", args).await {
            // A read: no side effect can be in doubt.
            Err(CallError::Unknown(e)) => return Err(CallError::NotSent(e)),
            other => other?,
        };
        Ok(decode_session_state(answer)?)
    }

    async fn session_send(&self, id: &str, text: &str) -> Result<(), CallError> {
        let args = json!({ "session_id": id, "text": text });
        match self.call("session_send", args).await? {
            ToolAnswer::Error(text) => Err(CallError::NotSent(format!("tm session_send: {text}"))),
            ToolAnswer::Json(v) => match v.get("sent").and_then(Value::as_bool) {
                Some(true) => Ok(()),
                _ => Err(CallError::Unknown(format!(
                    "tm session_send did not confirm submission (submit_state {})",
                    v.get("submit_state").unwrap_or(&Value::Null)
                ))),
            },
        }
    }
}

/// One decoded MCP `tools/call` result.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ToolAnswer {
    /// `isError: true`; the first text frame.
    Error(String),
    /// The first text frame parsed as JSON (`Null` when it is not JSON).
    Json(Value),
}

impl ToolAnswer {
    /// Decode `{"content":[{"type":"text","text":…}],"isError":…}`.
    /// Test: `super::tests::client_tests::tool_answers_decode`.
    pub(crate) fn from_result(result: &Value) -> Self {
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Self::Error(text.to_string());
        }
        Self::Json(serde_json::from_str(text).unwrap_or(Value::Null))
    }
}

/// Map a `session_activity` answer onto a [`TmSessionState`].
///
/// What: tm's "session not found" error, and the terminal states
/// `decommissioned` and `deleted`, are [`TmSessionState::Gone`]; `stopped`
/// and `errored` are [`TmSessionState::Parked`]; any other state is live. Any
/// other error is `NotSent`.
/// Test: `super::tests::client_tests::session_states_decode`.
pub(crate) fn decode_session_state(answer: ToolAnswer) -> Result<TmSessionState, CallError> {
    match answer {
        // trusty-mpm `ManagedError::SessionNotFound` renders this way.
        ToolAnswer::Error(text) if text.starts_with("session not found") => {
            Ok(TmSessionState::Gone { reason: text })
        }
        ToolAnswer::Error(text) => Err(CallError::NotSent(format!("tm session_activity: {text}"))),
        ToolAnswer::Json(v) => match v.get("state").and_then(Value::as_str) {
            Some(s @ ("decommissioned" | "deleted")) => Ok(TmSessionState::Gone {
                reason: format!("tm reports the session {s}"),
            }),
            Some(s @ ("stopped" | "errored")) => Ok(TmSessionState::Parked {
                state: s.to_string(),
            }),
            Some(_) => Ok(TmSessionState::Live),
            None => Err(CallError::NotSent(
                "tm session_activity answered without a state".into(),
            )),
        },
    }
}

/// trusty-code's `task.run` over the `tcode serve` socket.
pub struct UdsTc {
    /// The socket, or why it could not be resolved; the error is returned by
    /// the first call, so a tm handoff never fails on it.
    socket: Result<PathBuf, CallError>,
}

impl UdsTc {
    /// A client for `socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: Ok(socket.into()),
        }
    }

    /// A client for the shared `tcode serve` socket
    /// (`trusty_code::serve::uds::socket_path`). Dials nothing.
    pub fn shared() -> Self {
        Self {
            socket: trusty_code::serve::uds::socket_path()
                .map_err(|e| CallError::NotSent(format!("cannot resolve the tcode socket: {e:#}"))),
        }
    }
}

#[async_trait]
impl TcClient for UdsTc {
    async fn task_run(&self, task: &str, project: Option<&Path>) -> Result<String, CallError> {
        let mut params = json!({ "task_description": task });
        if let Some(project) = project {
            params["project"] = json!(project.display().to_string());
        }
        let client = UdsRpcClient::new(self.socket.clone()?);
        match client.call("task.run", params).await {
            Ok(v) => v
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or(CallError::Unknown(
                    "tcode task.run answered without a session_id".into(),
                )),
            Err(e) => Err(classify_tc_error(e)),
        }
    }
}

/// `NotSent` for a dial or encode failure and for the daemon's own error
/// answer; `Unknown` for a failure after the request may have been written.
/// Test: `super::tests::client_tests::tc_errors_classify`.
pub(crate) fn classify_tc_error(e: EngineError) -> CallError {
    let text = e.to_string();
    match e {
        EngineError::Rpc { .. } => CallError::NotSent(format!("tcode task.run: {text}")),
        EngineError::Transport { source, .. }
            if matches!(
                *source,
                UdsRpcError::Dial { .. } | UdsRpcError::Encode { .. }
            ) =>
        {
            CallError::NotSent(format!("tcode task.run: {text}"))
        }
        _ => CallError::Unknown(format!("tcode task.run: {text}")),
    }
}
