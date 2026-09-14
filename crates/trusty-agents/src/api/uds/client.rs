//! The in-repo caller's half of `agents.request` (#6433 slice 2).
//!
//! Why: before this, five call sites built their own `reqwest` client against
//! `http://127.0.0.1:<port>` — `runtime::session_cmds` (four routes),
//! `slack::relay`, `service`, `ctrl::repl::plain_cli` and
//! `runtime::mode_dispatch`. Each carried its own base-URL construction and its
//! own idea of the default port. One client removes all of that: there is no
//! address to construct and no port to default.
//!
//! What: [`ApiSocketClient`] dials the daemon's socket and issues one
//! `agents.request` per call. `get`/`post`/`delete` are the four verbs the
//! in-repo callers use; [`ApiSocketClient::request`] is the general form.
//!
//! Test: `client_round_trips_a_get`, `client_reports_a_dead_socket`,
//! `client_carries_a_json_post_body`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use super::wire::{HttpRequestFrame, HttpResponseFrame, METHOD_REQUEST};

/// Default budget for one request over the socket.
///
/// Why 30s and not the 500ms the health probe uses: these are real API calls —
/// `POST /api/ctrl/sessions` spawns a session, `POST /api/task` starts a
/// workflow — and the figure has to cover the handler, not just the transport.
/// A caller with a tighter deadline passes its own via
/// [`ApiSocketClient::with_timeout`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A client for one API daemon's socket.
///
/// Why hold the path rather than a connection: the framed protocol is one
/// exchange per connection (`uds::send_framed_request` dials, writes, reads,
/// closes), so there is no session to keep. Holding the path makes the client
/// cheap to clone and safe to hold across a daemon restart.
/// Test: see the module docs.
#[derive(Debug, Clone)]
pub struct ApiSocketClient {
    socket: PathBuf,
    timeout: Duration,
}

impl ApiSocketClient {
    /// A client for the API daemon of the project this process runs in.
    pub fn for_self_project() -> Self {
        Self::at(super::self_socket_path())
    }

    /// A client for the API daemon of `project_root`.
    pub fn for_project(project_root: &Path) -> Self {
        Self::at(super::api_socket_path(project_root))
    }

    /// A client for an explicit socket path.
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-request budget.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The socket this client dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Whether the daemon on this socket answers `agents.health`.
    pub async fn health_ok(&self) -> bool {
        super::health_ok(&self.socket).await
    }

    /// Issue one HTTP exchange over the socket.
    ///
    /// # Errors
    ///
    /// When the socket cannot be dialled (no daemon, wrong modes, wrong owner),
    /// when the exchange times out, or when the daemon answers a JSON-RPC error
    /// frame. A non-2xx HTTP status is NOT an error — it is a
    /// [`HttpResponseFrame`] with that status, because the caller decides what
    /// a 404 means (`session attach` treats it as "no such session").
    ///
    /// Test: `client_round_trips_a_get`, `client_reports_a_dead_socket`.
    pub async fn request(&self, frame: HttpRequestFrame) -> Result<HttpResponseFrame> {
        let path = frame.path.clone();
        let method = frame.method.clone();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": METHOD_REQUEST,
            "params": frame,
        });
        let response = trusty_common::uds::send_framed_request::<
            _,
            trusty_common::uds::server::RpcResponse,
        >(&self.socket, &request, self.timeout)
        .await
        .with_context(|| {
            format!(
                "calling {method} {path} on the trusty-agents API socket {}",
                self.socket.display()
            )
        })?;

        if let Some(error) = response.error {
            anyhow::bail!(
                "trusty-agents API refused {method} {path}: {} (code {})",
                error.message,
                error.code
            );
        }
        let result = response
            .result
            .with_context(|| format!("trusty-agents API answered {method} {path} with no result"))?;
        serde_json::from_value(result)
            .with_context(|| format!("decoding the response to {method} {path}"))
    }

    /// `GET path`.
    ///
    /// # Errors
    ///
    /// As [`ApiSocketClient::request`].
    pub async fn get(&self, path: &str) -> Result<HttpResponseFrame> {
        self.request(HttpRequestFrame::new("GET", path)).await
    }

    /// `DELETE path`.
    ///
    /// # Errors
    ///
    /// As [`ApiSocketClient::request`].
    pub async fn delete(&self, path: &str) -> Result<HttpResponseFrame> {
        self.request(HttpRequestFrame::new("DELETE", path)).await
    }

    /// `POST path` with a JSON body.
    ///
    /// # Errors
    ///
    /// As [`ApiSocketClient::request`], plus a serialization failure on `body`.
    pub async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Result<HttpResponseFrame> {
        let frame = HttpRequestFrame::new("POST", path)
            .with_json(body)
            .with_context(|| format!("serializing the request body for POST {path}"))?;
        self.request(frame).await
    }

    /// `POST path` with no body.
    ///
    /// # Errors
    ///
    /// As [`ApiSocketClient::request`].
    pub async fn post_empty(&self, path: &str) -> Result<HttpResponseFrame> {
        self.request(HttpRequestFrame::new("POST", path)).await
    }
}
