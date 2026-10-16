//! The Socket Mode transport seam (#9923 Q6).
//!
//! Why: the loop's claim/ack/effect order must be testable without Slack,
//! and the live legs need a socket server in the sandbox. The production
//! transport must reach only Slack: its API base is the constant
//! `SLACK_API_BASE`, never an environment variable.
//! What: [`SocketTransport`] opens a [`SocketConn`]. [`SlackTransport`]
//! resolves the app token through the host's `app_ref`
//! ([`super::token::app_token`]), calls `apps.connections.open` on the
//! constant base, checks the returned URL is a `wss://` Slack URL and
//! connects. [`LoopbackTransport`] connects to a `ws://127.0.0.1` URL with
//! no token; [`transport_for`] picks it only when the sandbox latch is set
//! and the sandbox home names one ([`fake_url`]). [`WsConn`] serves both.
//! Test: `fake_socket_server_is_unreachable_outside_the_sandbox`,
//! `ws_conn_acks_by_envelope_id_and_answers_pings`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::channels::slack_bot::SLACK_API_BASE;

/// How long `apps.connections.open` may take.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest a live connection may stay silent; Slack pings well inside it.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Opens one Socket Mode connection.
#[async_trait::async_trait]
pub trait SocketTransport: Send + Sync {
    /// A new connection, or why none opened (content-free).
    async fn connect(&self) -> Result<Box<dyn SocketConn>, String>;
}

/// One open Socket Mode connection.
#[async_trait::async_trait]
pub trait SocketConn: Send {
    /// The next text frame; `None` once the connection is closed.
    async fn recv(&mut self) -> Option<Result<String, String>>;
    /// Ack the envelope `envelope_id`.
    async fn ack(&mut self, envelope_id: &str) -> Result<(), String>;
}

/// The production transport: Slack's API base, the `app_ref` token.
#[derive(Debug, Clone)]
pub struct SlackTransport {
    home: Option<PathBuf>,
}

impl SlackTransport {
    /// The transport for the daemon home `home`.
    pub fn new(home: Option<PathBuf>) -> Self {
        Self { home }
    }
}

#[async_trait::async_trait]
impl SocketTransport for SlackTransport {
    async fn connect(&self) -> Result<Box<dyn SocketConn>, String> {
        let home = self.home.clone();
        // #9923 (plan item 2): the token comes from `app_ref` only; blocking
        // (Keychain), so on the blocking pool.
        let token = tokio::task::spawn_blocking(move || super::token::app_token(home.as_deref()))
            .await
            .map_err(|e| format!("the token task did not finish: {e}"))??;
        let client = reqwest::Client::builder()
            .timeout(OPEN_TIMEOUT)
            .build()
            .map_err(|_| "client_build".to_owned())?;
        let reply = client
            .post(format!("{SLACK_API_BASE}/apps.connections.open"))
            .bearer_auth(token.expose())
            .send()
            .await
            .map_err(|_| "apps.connections.open: transport".to_owned())?;
        let status = reply.status().as_u16();
        let body = reply
            .text()
            .await
            .map_err(|_| "apps.connections.open: body".to_owned())?;
        let url = socket_url(status, &body)?;
        WsConn::connect(&url).await.map(boxed)
    }
}

/// The `wss://` URL in an `apps.connections.open` reply.
///
/// What: a non-200, an undecodable body, `ok: false` (Slack's code kept to
/// `[a-z_]`), and a URL that is not `wss://` on a `slack.com` host are each
/// an error.
/// Test: `socket_url_accepts_only_a_slack_wss_url`.
pub fn socket_url(status: u16, body: &str) -> Result<String, String> {
    if status != 200 {
        return Err(format!("apps.connections.open: http_{status}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "apps.connections.open: undecodable".to_owned())?;
    if v.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let code: String = v
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("not_ok")
            .chars()
            .filter(|c| c.is_ascii_lowercase() || *c == '_')
            .take(64)
            .collect();
        return Err(format!("apps.connections.open: {code}"));
    }
    let url = v
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or("apps.connections.open: no url")?;
    let host = url
        .strip_prefix("wss://")
        .and_then(|rest| rest.split(['/', '?', ':']).next())
        .unwrap_or("");
    if host != "slack.com" && !host.ends_with(".slack.com") {
        return Err("apps.connections.open: not a Slack wss url".to_owned());
    }
    Ok(url.to_owned())
}

/// The sandbox-only transport: a loopback `ws://` server, no token.
#[derive(Debug, Clone)]
pub struct LoopbackTransport {
    url: String,
}

#[async_trait::async_trait]
impl SocketTransport for LoopbackTransport {
    async fn connect(&self) -> Result<Box<dyn SocketConn>, String> {
        WsConn::connect(&self.url).await.map(boxed)
    }
}

/// The file a sandbox home may hold naming a loopback fake server.
pub fn fake_url_path(home: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    )
    .join("channels")
    .join("socket_mode_fake_url")
}

/// The fake server's URL; `None` unless `sandboxed`.
///
/// Why: #9923 Q6: the fake is behind the same one-way latch as
/// `inbound_fake`; outside the sandbox the file is never read.
/// What: when `sandboxed` and `home` is known, the trimmed first line of
/// [`fake_url_path`] if it is `ws://127.0.0.1:<port>/…`; else `None`.
/// Test: `fake_socket_server_is_unreachable_outside_the_sandbox`.
pub fn fake_url(sandboxed: bool, home: Option<&Path>) -> Option<String> {
    if !sandboxed {
        return None;
    }
    let text = std::fs::read_to_string(fake_url_path(home?)).ok()?;
    let url = text.lines().next()?.trim();
    url.strip_prefix("ws://127.0.0.1:")
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .map(|_| url.to_owned())
}

/// The daemon's transport: the loopback fake in a sandbox that names one,
/// else Slack.
///
/// Test: `fake_socket_server_is_unreachable_outside_the_sandbox`.
pub fn transport_for(sandboxed: bool, home: Option<PathBuf>) -> Arc<dyn SocketTransport> {
    match fake_url(sandboxed, home.as_deref()) {
        Some(url) => {
            tracing::warn!("socket mode: sandbox fake server (#9923 Q6)");
            Arc::new(LoopbackTransport { url })
        }
        None => Arc::new(SlackTransport::new(home)),
    }
}

/// A WebSocket connection over `tokio-tungstenite`.
pub struct WsConn {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WsConn {
    /// Connect to `url`.
    pub async fn connect(url: &str) -> Result<Self, String> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|_| "websocket connect failed".to_owned())?;
        Ok(Self { ws })
    }
}

#[async_trait::async_trait]
impl SocketConn for WsConn {
    async fn recv(&mut self) -> Option<Result<String, String>> {
        loop {
            let next = match tokio::time::timeout(IDLE_TIMEOUT, self.ws.next()).await {
                Ok(next) => next,
                Err(_) => return Some(Err("socket idle past its timeout".to_owned())),
            };
            match next? {
                Ok(Message::Text(text)) => return Some(Ok(text)),
                Ok(Message::Ping(data)) => {
                    if self.ws.send(Message::Pong(data)).await.is_err() {
                        return Some(Err("pong failed".to_owned()));
                    }
                }
                Ok(Message::Close(_)) => return None,
                Ok(_) => {}
                Err(_) => return Some(Err("websocket read failed".to_owned())),
            }
        }
    }

    async fn ack(&mut self, envelope_id: &str) -> Result<(), String> {
        let body = serde_json::json!({ "envelope_id": envelope_id }).to_string();
        self.ws
            .send(Message::Text(body))
            .await
            .map_err(|_| "ack write failed".to_owned())
    }
}

fn boxed(conn: WsConn) -> Box<dyn SocketConn> {
    Box::new(conn)
}
