//! Slack Socket Mode bot gateway to the ctrl orchestrator (#418).
//!
//! Why: Mirrors the Telegram adapter (#264) for Slack — drives trusty-agents from
//! any Slack workspace via WebSocket Socket Mode, exposing the same
//! `ctrl::run_pm_task_with_history` PM loop that powers the local REPL and
//! the Telegram bot. Each channel gets its own `ChatSession` keyed by
//! channel id, so conversations from different channels don't mix.
//!
//! What: Socket Mode WebSocket (no public URL / inbound webhook required).
//! Slash commands `/slack-start`, `/slack-pair`, `/slack-connect`,
//! `/slack-clear`, `/slack-status` plus plain-text fallback dispatched to
//! ctrl. Responses are sent via `chat.postMessage` with mrkdwn formatting,
//! split at 3000-char boundaries on newline preference.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — socket lifecycle (`run_slack_bot`), session types, dispatch
//! - `rbac.rs` — per-user RBAC config + Virtual-CTO gating
//! - `pairing.rs` — pairing state machine + REPL-issued codes
//! - `handlers.rs` — slash/plain-text handlers + `chat.postMessage` senders
//! - `format.rs` — mrkdwn conversion + 3000-char chunking
//! - `tests.rs` — unit tests for the pure helpers above
//!
//! Test: Build with `cargo build`; unit-test pure functions (split_message,
//! markdown_to_mrkdwn, generate_pairing_code, verify_pair_attempt, sentinel
//! flow). Live verification requires a Slack workspace + app tokens.

mod format;
mod handlers;
mod pairing;
mod rbac;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

use crate::ctrl::ConversationTurn;

use handlers::{handle_command, handle_message};

// Re-export the public REPL-/dispatch-facing API so callers continue to use
// `crate::slack::{PendingPairs, new_pending_pairs, issue_repl_pairing_code,
// run_slack_bot, SlackRbacConfig, SlackUserConfig}` after the split.
pub use pairing::{
    PendingPairs, SENTINEL_PAIRING_CHANNEL_ID, issue_repl_pairing_code, new_pending_pairs,
};
pub use rbac::{SlackRbacConfig, SlackUserConfig};

/// Max processed envelopes tracked for dedup (LRU-style).
///
/// Why: Slack retries unacked events; we ACK immediately and then dispatch
/// asynchronously, so a duplicate envelope can arrive while ctrl is still
/// running. We keep the last N envelope ids to skip duplicates.
pub(super) const ENVELOPE_DEDUP_CAP: usize = 100;

/// Channel identifier (Slack channel ids are short strings like "C0123ABC",
/// but we hash them into i64 for cross-adapter parity with Telegram's
/// `ChatId`). We instead use the raw string here for fidelity, and reserve
/// `i64::MAX` as the sentinel for REPL-issued pairing codes — keyed by i64
/// for compatibility with the existing REPL pairing API.
pub type ChannelId = String;

/// Per-channel conversation state.
///
/// Why: Each Slack channel is a separate conversation with the ctrl PM. We
/// keep history per-channel so /slack-clear in one channel doesn't wipe
/// another, and /slack-connect can rebind a single channel to a different
/// project.
/// What: Tracks the active project path (defaults to launch path) and the
/// rolling list of `ConversationTurn`s passed to ctrl on each turn.
/// Test: Covered indirectly via `handle_message` exercising the SessionMap.
pub(super) struct ChatSession {
    pub(super) project_path: PathBuf,
    pub(super) history: Vec<ConversationTurn>,
    /// Active persona name for this channel (#480). Defaults to
    /// `SlackRbacConfig::default_persona` (`cto-assistant`); changed via
    /// `/slack-switch`.
    pub(super) active_persona: String,
    /// Resolved RBAC identity for this channel (#481). Set on the first
    /// message from a known user so it isn't re-looked-up per turn.
    pub(super) user_identity: Option<crate::rbac::UserIdentity>,
}

impl ChatSession {
    pub(super) fn new(project_path: PathBuf, default_persona: String) -> Self {
        Self {
            project_path,
            history: Vec::new(),
            active_persona: default_persona,
            user_identity: None,
        }
    }
}

/// Map of `ChannelId` -> per-channel session, shared across handlers.
pub(super) type SessionMap = Arc<Mutex<HashMap<ChannelId, ChatSession>>>;

/// Map of `ChannelId` -> instant the channel was paired, shared across
/// handlers. RwLock because reads dominate writes (every message reads, only
/// /slack-pair writes).
pub(super) type PairedChannels = Arc<RwLock<HashMap<ChannelId, Instant>>>;

/// Run the Slack bot in Socket Mode until SIGINT.
///
/// Why: Entry point wired to `--slack` in `main.rs`. Socket Mode uses an
/// outbound WebSocket so the bot can run from a developer's laptop or CI
/// runner without a public URL.
/// What: Exchanges `SLACK_APP_TOKEN` for a WSS URL via
/// `apps.connections.open`, opens the socket, loops on incoming envelopes
/// (ACK immediately, dispatch async), reconnects on disconnect.
/// Test: `cargo build` is the primary gate; live verification needs a real
/// Slack workspace.
pub async fn run_slack_bot(
    project_path: PathBuf,
    pending: PendingPairs,
    rbac: Arc<SlackRbacConfig>,
) -> Result<()> {
    let app_token = std::env::var("SLACK_APP_TOKEN").map_err(|_| {
        anyhow!(
            "SLACK_APP_TOKEN not set. Add it to .env.local or export it before running --slack."
        )
    })?;
    let bot_token = std::env::var("SLACK_BOT_TOKEN").map_err(|_| {
        anyhow!(
            "SLACK_BOT_TOKEN not set. Add it to .env.local or export it before running --slack."
        )
    })?;

    if !app_token.starts_with("xapp-") {
        warn!(
            "SLACK_APP_TOKEN does not start with 'xapp-' — Socket Mode requires an app-level token"
        );
    }
    if !bot_token.starts_with("xoxb-") {
        warn!(
            "SLACK_BOT_TOKEN does not start with 'xoxb-' — chat.postMessage requires a bot token"
        );
    }

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| anyhow!("failed to build slack HTTP client: {}", e))?;

    let project_path = Arc::new(project_path);
    let sessions: SessionMap = Arc::new(Mutex::new(HashMap::new()));
    let paired: PairedChannels = Arc::new(RwLock::new(HashMap::new()));
    let dedup: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(ENVELOPE_DEDUP_CAP)));

    info!(
        project = %project_path.display(),
        "Starting Slack bot in Socket Mode"
    );

    // Reconnect loop: Slack rotates connections periodically and may send
    // `disconnect` to gracefully ask us to reconnect. On any error we wait
    // briefly and retry — same shape as a long-poll resilience loop.
    loop {
        match open_socket_and_run(
            &http,
            &app_token,
            &bot_token,
            Arc::clone(&project_path),
            Arc::clone(&sessions),
            Arc::clone(&paired),
            Arc::clone(&pending),
            Arc::clone(&dedup),
            Arc::clone(&rbac),
        )
        .await
        {
            Ok(()) => {
                info!("Slack socket closed cleanly; reconnecting");
            }
            Err(e) => {
                error!(error = %e, "Slack socket error; reconnecting in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// One iteration of the Socket Mode connection. Returns Ok if Slack asked
/// us to reconnect; Err on transport / API failures.
#[allow(clippy::too_many_arguments)]
async fn open_socket_and_run(
    http: &reqwest::Client,
    app_token: &str,
    bot_token: &str,
    project_path: Arc<PathBuf>,
    sessions: SessionMap,
    paired: PairedChannels,
    pending: PendingPairs,
    dedup: Arc<Mutex<VecDeque<String>>>,
    rbac: Arc<SlackRbacConfig>,
) -> Result<()> {
    let wss_url = open_connection(http, app_token).await?;
    info!("Slack Socket Mode connected");

    let (ws_stream, _) = tokio_tungstenite::connect_async(&wss_url)
        .await
        .map_err(|e| anyhow!("failed to connect to Slack WSS: {}", e))?;
    let (mut writer, mut reader) = ws_stream.split();

    while let Some(frame) = reader.next().await {
        let frame = frame.map_err(|e| anyhow!("ws read error: {}", e))?;
        let text = match frame {
            WsMessage::Text(t) => t,
            WsMessage::Ping(p) => {
                writer
                    .send(WsMessage::Pong(p))
                    .await
                    .map_err(|e| anyhow!("ws pong send failed: {}", e))?;
                continue;
            }
            WsMessage::Close(_) => {
                info!("Slack closed WebSocket");
                return Ok(());
            }
            _ => continue,
        };

        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, raw = %text, "slack: bad envelope JSON");
                continue;
            }
        };
        let env_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        debug!(env_type, "slack envelope received");

        match env_type {
            "hello" => continue,
            "disconnect" => {
                info!("Slack requested disconnect (rotation); reconnecting");
                return Ok(());
            }
            "events_api" | "slash_commands" | "interactive" => {
                // ACK first, dispatch second.
                if let Some(env_id) = value.get("envelope_id").and_then(|v| v.as_str()) {
                    let ack = json!({ "envelope_id": env_id }).to_string();
                    if let Err(e) = writer.send(WsMessage::Text(ack)).await {
                        warn!(error = %e, "slack ACK send failed");
                    }
                    if !dedup_check_and_record(&dedup, env_id).await {
                        debug!(env_id, "duplicate envelope; skipping dispatch");
                        continue;
                    }
                }
                let bot_token = bot_token.to_string();
                let project_path = Arc::clone(&project_path);
                let sessions = Arc::clone(&sessions);
                let paired = Arc::clone(&paired);
                let pending = Arc::clone(&pending);
                let rbac = Arc::clone(&rbac);
                tokio::spawn(async move {
                    if let Err(e) = dispatch_envelope(
                        value,
                        &bot_token,
                        project_path,
                        sessions,
                        paired,
                        pending,
                        rbac,
                    )
                    .await
                    {
                        warn!(error = %e, "slack dispatch failed");
                    }
                });
            }
            other => {
                debug!(env_type = other, "slack: unhandled envelope type");
            }
        }
    }

    Ok(())
}

/// Returns false if envelope id is a duplicate (already processed).
pub(super) async fn dedup_check_and_record(
    dedup: &Arc<Mutex<VecDeque<String>>>,
    env_id: &str,
) -> bool {
    let mut q = dedup.lock().await;
    if q.iter().any(|s| s == env_id) {
        return false;
    }
    if q.len() >= ENVELOPE_DEDUP_CAP {
        q.pop_front();
    }
    q.push_back(env_id.to_string());
    true
}

/// Exchange `SLACK_APP_TOKEN` for a single-use WebSocket URL.
async fn open_connection(http: &reqwest::Client, app_token: &str) -> Result<String> {
    let resp = http
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send()
        .await
        .map_err(|e| anyhow!("apps.connections.open request failed: {}", e))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("apps.connections.open: bad json (status {status}): {e}"))?;
    if !body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(anyhow!(
            "apps.connections.open failed: {} (status {})",
            err,
            status
        ));
    }
    body.get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("apps.connections.open: missing 'url' field"))
}

/// Dispatch a single Slack envelope (either an event or a slash command).
#[allow(clippy::too_many_arguments)]
async fn dispatch_envelope(
    envelope: Value,
    bot_token: &str,
    project_path: Arc<PathBuf>,
    sessions: SessionMap,
    paired: PairedChannels,
    pending: PendingPairs,
    rbac: Arc<SlackRbacConfig>,
) -> Result<()> {
    let env_type = envelope.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match env_type {
        "events_api" => {
            let event = envelope
                .get("payload")
                .and_then(|p| p.get("event"))
                .cloned()
                .unwrap_or(Value::Null);
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            // Only react to plain user messages; skip bot messages and edits.
            if event_type != "message" {
                return Ok(());
            }
            if event.get("bot_id").is_some() || event.get("subtype").is_some() {
                return Ok(());
            }
            let channel = match event.get("channel").and_then(|v| v.as_str()) {
                Some(c) => c.to_string(),
                None => return Ok(()),
            };
            let text = event
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                return Ok(());
            }
            let thread_ts = event
                .get("thread_ts")
                .and_then(|v| v.as_str())
                .or_else(|| event.get("ts").and_then(|v| v.as_str()))
                .map(|s| s.to_string());
            let user_id = event
                .get("user")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            handle_message(
                bot_token,
                channel,
                user_id,
                text,
                thread_ts,
                sessions,
                project_path,
                paired,
                rbac,
            )
            .await
        }
        "slash_commands" => {
            let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
            let command = payload
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let channel = payload
                .get("channel_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arg_text = payload
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let user_id = payload
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            handle_command(
                bot_token,
                channel,
                user_id,
                command,
                arg_text,
                sessions,
                project_path,
                paired,
                pending,
                rbac,
            )
            .await
        }
        _ => Ok(()),
    }
}
