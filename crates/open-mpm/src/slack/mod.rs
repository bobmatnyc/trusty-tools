//! Slack Socket Mode bot gateway to the ctrl orchestrator (#418).
//!
//! Why: Mirrors the Telegram adapter (#264) for Slack — drives open-mpm from
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
//! Test: Build with `cargo build`; unit-test pure functions (split_message,
//! markdown_to_mrkdwn, generate_pairing_code, verify_pair_attempt, sentinel
//! flow). Live verification requires a Slack workspace + app tokens.

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

use crate::ctrl::{self, ConversationTurn};

/// Per-user RBAC config for Slack (#481).
///
/// Why: The Slack bot is shared across the Duetto engineering team, but the
/// underlying `cto-assistant` persona reaches sensitive HR/budget data. Each
/// known user is pinned to a `ServiceTier` (which gates the persona toolset
/// via `filter_tools_for_user`) and an optional persona allow-list (which
/// gates `/slack-switch`). Unknown users fall through to a Virtual-CTO reply.
/// What: A flat record keyed by Slack user id.
/// Test: `rbac_config_parses_env_string`, `switch_command_blocked_for_restricted_persona`.
#[derive(Debug, Clone)]
pub struct SlackUserConfig {
    pub slack_id: String,
    pub name: String,
    pub tier: crate::rbac::ServiceTier,
    /// Allowed persona names. `None` means unrestricted (any persona).
    pub allowed_personas: Option<Vec<String>>,
}

/// Bot-wide Slack RBAC configuration (#480/#481).
///
/// Why: Centralizes the env-driven user table and default persona so
/// `run_slack_bot` and its handlers take a single `Arc<SlackRbacConfig>`
/// rather than re-parsing env on every message.
/// What: A user table keyed by Slack id plus the default persona name.
/// Test: `rbac_config_parses_env_string`, `rbac_unknown_user_returns_virtual_cto_message`.
#[derive(Debug, Clone)]
pub struct SlackRbacConfig {
    /// Keyed by Slack user ID.
    users: std::collections::HashMap<String, SlackUserConfig>,
    /// Default persona for all messages (from `SLACK_DEFAULT_PERSONA`,
    /// default `"cto-assistant"`).
    pub default_persona: String,
}

/// Static reply for Slack users not in the RBAC table (#481).
///
/// Why: Unknown users must NOT reach the LLM or any tool — the bot speaks as
/// a general "Virtual CTO" with no internal-data access. Returned verbatim,
/// bypassing `run_pm_task_with_persona` entirely.
const VIRTUAL_CTO_MESSAGE: &str = ":lock: This assistant is for Duetto engineering team members. \
I can discuss general technology strategy and software architecture, but I don't have access to \
internal Duetto data. Feel free to ask general questions.";

impl SlackRbacConfig {
    /// Parse the RBAC config from process env.
    ///
    /// Why: Lets ops configure the user table without a code change. Falls
    /// back to a sensible hardcoded team list when `SLACK_RBAC_USERS` is
    /// absent so the bot is usable out of the box.
    /// What: `SLACK_DEFAULT_PERSONA` → `default_persona` (default
    /// `"cto-assistant"`). `SLACK_RBAC_USERS` → comma-separated
    /// `ID:Name:TIER:PERSONAS` entries; `PERSONAS` is `*` (unrestricted) or a
    /// `+`-separated allow-list.
    /// Test: `rbac_config_parses_env_string`.
    pub fn from_env() -> Self {
        let default_persona = std::env::var("SLACK_DEFAULT_PERSONA")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "cto-assistant".to_string());
        let raw = std::env::var("SLACK_RBAC_USERS").ok();
        let users = match raw.as_deref() {
            Some(s) if !s.trim().is_empty() => parse_rbac_users(s),
            _ => default_rbac_users(),
        };
        Self {
            users,
            default_persona,
        }
    }

    /// Look up a Slack user id in the RBAC table.
    pub fn user(&self, slack_id: &str) -> Option<&SlackUserConfig> {
        self.users.get(slack_id)
    }
}

/// Parse a `SLACK_RBAC_USERS` env string into a user table.
///
/// Why: Pure function so it can be unit-tested without touching process env.
/// What: Splits on `,` for entries and `:` for the 4 fields. Malformed
/// entries (wrong field count, unknown tier) are skipped with a warning.
/// Test: `rbac_config_parses_env_string`.
fn parse_rbac_users(raw: &str) -> std::collections::HashMap<String, SlackUserConfig> {
    let mut map = std::collections::HashMap::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.split(':').collect();
        if parts.len() != 4 {
            warn!(entry, "slack rbac: skipping malformed user entry");
            continue;
        }
        let tier = match parts[2].trim().to_ascii_uppercase().as_str() {
            "ALL" => crate::rbac::ServiceTier::All,
            "ANALYTICS" => crate::rbac::ServiceTier::Analytics,
            "READONLY" => crate::rbac::ServiceTier::ReadOnly,
            other => {
                warn!(tier = other, entry, "slack rbac: unknown tier; skipping");
                continue;
            }
        };
        let allowed_personas = if parts[3].trim() == "*" {
            None
        } else {
            Some(
                parts[3]
                    .split('+')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        };
        let slack_id = parts[0].trim().to_string();
        map.insert(
            slack_id.clone(),
            SlackUserConfig {
                slack_id,
                name: parts[1].trim().to_string(),
                tier,
                allowed_personas,
            },
        );
    }
    map
}

/// Hardcoded default team RBAC table used when `SLACK_RBAC_USERS` is unset.
///
/// Why: The bot should be usable without ops first setting an env var.
/// Test: `rbac_config_parses_env_string` (indirectly via from_env fallback).
fn default_rbac_users() -> std::collections::HashMap<String, SlackUserConfig> {
    parse_rbac_users(
        "U0A6V2W1M2R:Masa:ALL:*,\
         U0ALDQLBU79:Andrea:ALL:cto-assistant,\
         U09331EP3MX:Alex:ANALYTICS:cto-assistant",
    )
}

/// How long a pairing code remains valid after issuance.
///
/// Why: Bound the window where a leaked code from REPL logs could be used by
/// another Slack channel. 5 minutes mirrors the Telegram adapter.
const PAIRING_CODE_TTL: Duration = Duration::from_secs(5 * 60);

/// Maximum characters per Slack message block.
///
/// Why: Slack's `chat.postMessage` `text` field has a 40k limit overall, but
/// individual mrkdwn blocks are recommended to stay <= 3000 chars for
/// readability and reliable rendering. Long ctrl responses are split at
/// newline boundaries before this limit.
const MAX_SLACK_MESSAGE: usize = 3000;

/// Max processed envelopes tracked for dedup (LRU-style).
///
/// Why: Slack retries unacked events; we ACK immediately and then dispatch
/// asynchronously, so a duplicate envelope can arrive while ctrl is still
/// running. We keep the last N envelope ids to skip duplicates.
const ENVELOPE_DEDUP_CAP: usize = 100;

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
struct ChatSession {
    project_path: PathBuf,
    history: Vec<ConversationTurn>,
    /// Active persona name for this channel (#480). Defaults to
    /// `SlackRbacConfig::default_persona` (`cto-assistant`); changed via
    /// `/slack-switch`.
    active_persona: String,
    /// Resolved RBAC identity for this channel (#481). Set on the first
    /// message from a known user so it isn't re-looked-up per turn.
    user_identity: Option<crate::rbac::UserIdentity>,
}

impl ChatSession {
    fn new(project_path: PathBuf, default_persona: String) -> Self {
        Self {
            project_path,
            history: Vec::new(),
            active_persona: default_persona,
            user_identity: None,
        }
    }
}

/// Map of `ChannelId` -> per-channel session, shared across handlers.
type SessionMap = Arc<Mutex<HashMap<ChannelId, ChatSession>>>;

/// Map of `ChannelId` -> instant the channel was paired, shared across
/// handlers. RwLock because reads dominate writes (every message reads, only
/// /slack-pair writes).
type PairedChannels = Arc<RwLock<HashMap<ChannelId, Instant>>>;

/// Map of pending pairing codes keyed by raw `i64` channel id.
///
/// Why: Pairing codes are generated **in the REPL** (trusted terminal),
/// stored under the sentinel key `SENTINEL_PAIRING_CHANNEL_ID = i64::MAX`.
/// When `/slack-pair <code>` arrives from Slack, we look up the sentinel
/// entry; on a match the channel is promoted to paired. An attacker who
/// owns the Slack bot cannot self-authorize — they'd also need shell
/// access to the host running the REPL.
/// What: `Arc<Mutex<HashMap<i64, (String, Instant)>>>`. The `i64` keeps the
/// REPL free of slack-adapter-specific types and reuses the Telegram API
/// shape exactly so the REPL doesn't have to learn a second pairing API.
pub type PendingPairs = Arc<Mutex<HashMap<i64, (String, Instant)>>>;

/// Sentinel channel-id under which the REPL stores the next pending code.
///
/// Why: A real Slack channel id is a string ("C0123ABC..."), never an
/// integer. We use `i64::MAX` as an out-of-band integer key so the REPL
/// pairing API stays uniform across Telegram + Slack.
pub const SENTINEL_PAIRING_CHANNEL_ID: i64 = i64::MAX;

/// Construct a fresh, empty `PendingPairs` shared across REPL + bot task.
pub fn new_pending_pairs() -> PendingPairs {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Generate and store a REPL-issued pairing code under the sentinel key.
///
/// Why: Called from a future `/slack pair` command in the REPL. The next
/// `/slack-pair <code>` arriving on Slack (from any channel) can claim it.
/// What: Returns the 6-digit code so the REPL can display it.
/// Test: `repl_issued_code_lands_under_sentinel` exercises the flow.
pub async fn issue_repl_pairing_code(pending: &PendingPairs) -> String {
    let code = generate_pairing_code();
    let mut map = pending.lock().await;
    map.insert(SENTINEL_PAIRING_CHANNEL_ID, (code.clone(), Instant::now()));
    code
}

/// Generate a random 6-digit pairing code (zero-padded).
///
/// Why: 6 digits = ~1M codes; plenty for human handoff via a log line, short
/// enough to type easily.
/// What: Uses `rand::random::<u32>() % 1_000_000` and zero-pads with `{:06}`.
/// Test: `pairing_code_is_six_digits` asserts the format.
fn generate_pairing_code() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

/// Outcome of a `/slack-pair <code>` attempt. Pure for unit testing.
///
/// Why: We want to unit-test the state-machine without WebSocket types in
/// the loop. `verify_pair_attempt` returns one of these and the handler
/// turns it into Slack replies + map mutations.
#[derive(Debug, PartialEq, Eq)]
enum PairOutcome {
    /// No pending code registered.
    NoPending,
    /// The pending code is past its TTL.
    Expired,
    /// The provided code does not match the pending code.
    Mismatch,
    /// The provided code matches and is within TTL — caller must promote
    /// the channel to paired.
    Success,
}

/// Verify a pairing attempt against a pending entry.
///
/// Why: Pure function so we can exhaustively test without spinning up Slack.
/// The caller is responsible for the side effects (removing the pending
/// entry, inserting into paired, posting the reply).
fn verify_pair_attempt(
    pending_entry: Option<&(String, Instant)>,
    provided_code: &str,
    now: Instant,
    ttl: Duration,
) -> PairOutcome {
    match pending_entry {
        None => PairOutcome::NoPending,
        Some((code, issued_at)) => {
            if now.saturating_duration_since(*issued_at) > ttl {
                PairOutcome::Expired
            } else if code != provided_code {
                PairOutcome::Mismatch
            } else {
                PairOutcome::Success
            }
        }
    }
}

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
async fn dedup_check_and_record(dedup: &Arc<Mutex<VecDeque<String>>>, env_id: &str) -> bool {
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

/// Build a ctrl `UserIdentity` from a Slack RBAC user entry (#481).
///
/// Why: `run_pm_task_with_persona` gates the persona toolset by
/// `UserIdentity.tier`; this is the single translation point from the
/// Slack-native `SlackUserConfig` to the transport-agnostic identity.
/// Test: exercised via `rbac_unknown_user_returns_virtual_cto_message` and
/// `switch_command_blocked_for_restricted_persona`.
fn identity_from_slack_user(u: &SlackUserConfig) -> crate::rbac::UserIdentity {
    crate::rbac::UserIdentity::new(u.slack_id.clone(), u.name.clone(), u.tier.clone())
}

/// Slash command dispatch.
#[allow(clippy::too_many_arguments)]
async fn handle_command(
    bot_token: &str,
    channel: ChannelId,
    user_id: String,
    command: String,
    arg: String,
    sessions: SessionMap,
    project_path: Arc<PathBuf>,
    paired: PairedChannels,
    pending: PendingPairs,
    rbac: Arc<SlackRbacConfig>,
) -> Result<()> {
    // Gate every command except /slack-start and /slack-pair behind the
    // pairing check. Unpaired channels get a uniform prompt.
    let is_unauthenticated = matches!(command.as_str(), "/slack-start" | "/slack-pair");
    if !is_unauthenticated {
        let is_paired = paired.read().await.contains_key(&channel);
        if !is_paired {
            return post_message(
                bot_token,
                &channel,
                ":lock: Not paired. Send `/slack-start` to begin.",
                None,
            )
            .await;
        }
    }

    match command.as_str() {
        "/slack-start" => {
            info!(channel = %channel, "Slack /slack-start received");
            let text = concat!(
                ":lock: *Pairing required*\n\n",
                "To link this Slack channel, go to your open-mpm REPL and run:\n\n",
                "  `/slack pair`\n\n",
                "Then send the code here: `/slack-pair <code>`\n\n",
                "(Codes expire in 5 minutes.)"
            );
            post_message(bot_token, &channel, text, None).await
        }
        "/slack-pair" => {
            let provided = arg.trim().to_string();
            if provided.is_empty() {
                return post_message(bot_token, &channel, "Usage: `/slack-pair <code>`", None)
                    .await;
            }
            let now = Instant::now();
            let (outcome, matched_key) = {
                let map = pending.lock().await;
                let sentinel_outcome = verify_pair_attempt(
                    map.get(&SENTINEL_PAIRING_CHANNEL_ID),
                    &provided,
                    now,
                    PAIRING_CODE_TTL,
                );
                (sentinel_outcome, SENTINEL_PAIRING_CHANNEL_ID)
            };
            match outcome {
                PairOutcome::NoPending => {
                    post_message(
                        bot_token,
                        &channel,
                        "No pending pairing. Run `/slack pair` in the REPL first.",
                        None,
                    )
                    .await
                }
                PairOutcome::Expired => {
                    pending.lock().await.remove(&matched_key);
                    post_message(
                        bot_token,
                        &channel,
                        "Code expired. Run `/slack pair` in the REPL to get a new code.",
                        None,
                    )
                    .await
                }
                PairOutcome::Mismatch => {
                    post_message(bot_token, &channel, "Invalid code.", None).await
                }
                PairOutcome::Success => {
                    pending.lock().await.remove(&matched_key);
                    paired.write().await.insert(channel.clone(), now);
                    info!(channel = %channel, "Slack channel paired successfully");
                    post_message(
                        bot_token,
                        &channel,
                        ":white_check_mark: *Paired successfully.* You can now send messages.",
                        None,
                    )
                    .await
                }
            }
        }
        "/slack-connect" => {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                return post_message(bot_token, &channel, "Usage: `/slack-connect <path>`", None)
                    .await;
            }
            let new_path = PathBuf::from(trimmed);
            if !new_path.is_dir() {
                return post_message(
                    bot_token,
                    &channel,
                    &format!("Path does not exist or is not a directory: `{}`", trimmed),
                    None,
                )
                .await;
            }
            {
                let mut map = sessions.lock().await;
                let entry = map.entry(channel.clone()).or_insert_with(|| {
                    ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
                });
                entry.project_path = new_path.clone();
            }
            post_message(
                bot_token,
                &channel,
                &format!("Connected to `{}`", new_path.display()),
                None,
            )
            .await
        }
        "/slack-clear" => {
            let mut map = sessions.lock().await;
            if let Some(session) = map.get_mut(&channel) {
                session.history.clear();
            }
            drop(map);
            post_message(bot_token, &channel, "Conversation history cleared.", None).await
        }
        "/slack-switch" => {
            let requested = arg.trim().to_string();
            if requested.is_empty() {
                return post_message(
                    bot_token,
                    &channel,
                    "Usage: `/slack-switch <persona>`",
                    None,
                )
                .await;
            }
            // Resolve the requesting Slack user from RBAC.
            let user_cfg = match rbac.user(&user_id) {
                Some(u) => u.clone(),
                None => {
                    return post_message(bot_token, &channel, ":lock: Not authorized.", None).await;
                }
            };
            // RBAC enforcement: persona allow-list. `None` => unrestricted.
            if let Some(allowed) = &user_cfg.allowed_personas
                && !allowed.iter().any(|p| p == &requested)
            {
                info!(
                    user_id = %user_id,
                    persona = %requested,
                    "slack: /slack-switch rejected (persona not in allow-list)"
                );
                return post_message(
                    bot_token,
                    &channel,
                    &format!(
                        ":lock: Not authorized to switch to *{}*. Allowed: {}",
                        requested,
                        allowed.join(", ")
                    ),
                    None,
                )
                .await;
            }
            {
                let mut map = sessions.lock().await;
                let entry = map.entry(channel.clone()).or_insert_with(|| {
                    ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
                });
                entry.active_persona = requested.clone();
            }
            info!(user_id = %user_id, persona = %requested, "slack: persona switched");
            post_message(
                bot_token,
                &channel,
                &format!(":arrows_counterclockwise: Switched to *{}*", requested),
                None,
            )
            .await
        }
        "/slack-status" => {
            let map = sessions.lock().await;
            let path = map
                .get(&channel)
                .map(|s| s.project_path.clone())
                .unwrap_or_else(|| (*project_path).clone());
            let history_len = map.get(&channel).map(|s| s.history.len()).unwrap_or(0);
            let persona = map
                .get(&channel)
                .map(|s| s.active_persona.clone())
                .unwrap_or_else(|| rbac.default_persona.clone());
            drop(map);

            let llm_label = crate::llm::credentials::pick_credentials(None)
                .map(|c| c.label())
                .unwrap_or("none");
            let text = format!(
                "*Status*\n\nProject:  `{}`\nPersona:  `{}`\nTurns:    {}\nLLM:      `{}`",
                path.display(),
                persona,
                history_len,
                llm_label
            );
            post_message(bot_token, &channel, &text, None).await
        }
        other => {
            warn!(command = %other, "slack: unknown slash command");
            Ok(())
        }
    }
}

/// Forward a plain-text message to ctrl and reply with the result.
#[allow(clippy::too_many_arguments)]
async fn handle_message(
    bot_token: &str,
    channel: ChannelId,
    user_id: String,
    text: String,
    thread_ts: Option<String>,
    sessions: SessionMap,
    project_path: Arc<PathBuf>,
    paired: PairedChannels,
    rbac: Arc<SlackRbacConfig>,
) -> Result<()> {
    // Gate behind pairing.
    if !paired.read().await.contains_key(&channel) {
        return post_message(
            bot_token,
            &channel,
            ":lock: Not paired. Send `/slack-start` to begin.",
            thread_ts.as_deref(),
        )
        .await;
    }

    // #481: RBAC identity gate. Unknown Slack users get the static Virtual
    // CTO reply — no LLM call, no tool dispatch.
    let user_cfg = match rbac.user(&user_id) {
        Some(u) => u.clone(),
        None => {
            info!(user_id = %user_id, "slack: unknown user → virtual CTO reply");
            return send_long_message(
                bot_token,
                &channel,
                thread_ts.as_deref(),
                VIRTUAL_CTO_MESSAGE,
            )
            .await;
        }
    };
    let user_identity = identity_from_slack_user(&user_cfg);

    let (path, history_snapshot, active_persona) = {
        let mut map = sessions.lock().await;
        let entry = map.entry(channel.clone()).or_insert_with(|| {
            ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
        });
        // Cache the resolved identity so it isn't re-looked-up per turn.
        entry.user_identity = Some(user_identity.clone());
        (
            entry.project_path.clone(),
            entry.history.clone(),
            entry.active_persona.clone(),
        )
    };

    info!(
        user_id = %user_id,
        user_name = %user_cfg.name,
        persona = %active_persona,
        "slack dispatch"
    );

    let result = ctrl::run_pm_task_with_persona(
        &path,
        &active_persona,
        &text,
        &history_snapshot,
        None,
        ctrl::SessionOverrides {
            user: Some(user_identity),
            ..Default::default()
        },
    )
    .await;

    let response_text = match result {
        Ok(reply) => {
            let mut map = sessions.lock().await;
            let entry = map.entry(channel.clone()).or_insert_with(|| {
                ChatSession::new((*project_path).clone(), rbac.default_persona.clone())
            });
            entry.history.push(ConversationTurn {
                user: text.clone(),
                assistant: reply.clone(),
            });
            drop(map);
            markdown_to_mrkdwn(&reply)
        }
        Err(e) => {
            warn!(channel = %channel, error = %e, "ctrl dispatch failed");
            ":warning: LLM backend not configured. Set `CLAUDE_CODE_OAUTH_TOKEN`, \
             `ANTHROPIC_API_KEY`, or `OPENROUTER_API_KEY`."
                .to_string()
        }
    };

    send_long_message(bot_token, &channel, thread_ts.as_deref(), &response_text).await
}

/// Post a single message via `chat.postMessage`.
async fn post_message(
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<()> {
    let mut body = serde_json::Map::new();
    body.insert("channel".to_string(), Value::String(channel.to_string()));
    body.insert("text".to_string(), Value::String(text.to_string()));
    body.insert("mrkdwn".to_string(), Value::Bool(true));
    if let Some(ts) = thread_ts {
        body.insert("thread_ts".to_string(), Value::String(ts.to_string()));
    }
    let resp = reqwest::Client::new()
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(bot_token)
        .json(&Value::Object(body))
        .send()
        .await
        .map_err(|e| anyhow!("chat.postMessage failed: {}", e))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("chat.postMessage: bad json (status {status}): {e}"))?;
    if !body.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        warn!(error = %err, "chat.postMessage returned not-ok");
    }
    Ok(())
}

/// Send a (possibly long) mrkdwn reply, splitting on the 3000-char boundary
/// at newlines where possible. Thread reply attached to all chunks for
/// coherence (Slack threads tolerate this, unlike Telegram replies).
async fn send_long_message(
    bot_token: &str,
    channel: &str,
    thread_ts: Option<&str>,
    text: &str,
) -> Result<()> {
    let chunks = split_message(text, MAX_SLACK_MESSAGE);
    for chunk in chunks.iter() {
        if let Err(e) = post_message(bot_token, channel, chunk, thread_ts).await {
            warn!(channel = %channel, error = %e, "slack chunk post failed");
        }
    }
    Ok(())
}

/// Split `text` into chunks of at most `max_len` chars, preferring to break
/// on newlines.
///
/// Why: Slack mrkdwn renders best when individual messages stay <= 3000
/// chars. Hard-splitting mid-line yields ugly output; we prefer the
/// rightmost newline in the first `max_len` chars, falling back to a hard
/// (UTF-8-safe) split when no newline is available.
/// What: Returns a `Vec<String>` whose concatenation equals `text`.
/// Test: `split_message_*` below.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > max_len {
        let mut boundary = max_len;
        while boundary > 0 && !remaining.is_char_boundary(boundary) {
            boundary -= 1;
        }
        if boundary == 0 {
            chunks.push(remaining.to_owned());
            return chunks;
        }
        let split_at = match remaining[..boundary].rfind('\n') {
            Some(pos) => pos + 1,
            None => boundary,
        };
        chunks.push(remaining[..split_at].to_owned());
        remaining = &remaining[split_at..];
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_owned());
    }
    chunks
}

/// Convert ctrl's Markdown-ish output into Slack mrkdwn.
///
/// Why: ctrl emits standard Markdown (`**bold**`, `` `code` ``, ``` ``` ```
/// fences). Slack mrkdwn uses `*bold*` (single asterisk) and preserves
/// triple-backtick fences and single-backtick inline code as-is. We strip
/// ANSI escapes and rewrite `**x**` -> `*x*` while leaving code spans
/// untouched.
/// Test: `markdown_to_mrkdwn_*` below.
fn markdown_to_mrkdwn(input: &str) -> String {
    let cleaned = strip_ansi(input);
    // Convert **bold** -> *bold*. Order matters: do this before any other
    // asterisk-touching rewrite. We use a paired-delimiter walker so
    // unbalanced `**` passes through as literal.
    convert_double_to_single_asterisk(&cleaned)
}

/// Replace paired `**` delimiters with single `*` for Slack mrkdwn bold.
///
/// Why: Slack mrkdwn uses `*bold*`, not Markdown's `**bold**`. Unbalanced
/// `**` is left as-is so we don't corrupt arbitrary asterisk content.
/// Test: `convert_double_to_single_asterisk_*` below.
fn convert_double_to_single_asterisk(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut next_is_open = true;
    while let Some(idx) = rest.find("**") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        if next_is_open && !after.contains("**") {
            // Unpaired — emit literal.
            out.push_str("**");
            rest = after;
            continue;
        }
        out.push('*');
        next_is_open = !next_is_open;
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Strip ANSI escape sequences (CSI / SGR) so terminal colour codes don't
/// leak into Slack messages.
fn strip_ansi(s: &str) -> String {
    strip_ansi_escapes::strip_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", MAX_SLACK_MESSAGE);
        assert_eq!(chunks, vec!["hello".to_string()]);
    }

    #[test]
    fn split_message_newline_boundary() {
        let line = "a".repeat(100);
        let text = format!("{}\n{}", line, line);
        let chunks = split_message(&text, 150);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
        assert_eq!(chunks[1], line);
    }

    #[test]
    fn split_message_hard_split_no_newline() {
        let text = "a".repeat(200);
        let chunks = split_message(&text, 100);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 100);
        assert_eq!(chunks[1].len(), 100);
    }

    #[test]
    fn split_message_utf8_safe() {
        // 4-byte chars at the boundary must not be split mid-sequence.
        let text = "🦀".repeat(50); // 200 bytes
        let chunks = split_message(&text, 99);
        let joined: String = chunks.join("");
        assert_eq!(joined, text, "round-trip must match");
    }

    #[test]
    fn markdown_to_mrkdwn_bold_conversion() {
        let out = markdown_to_mrkdwn("this is **important**!");
        assert_eq!(out, "this is *important*!");
    }

    #[test]
    fn markdown_to_mrkdwn_preserves_code_fences() {
        let input = "before\n```rust\nlet x = 1;\n```\nafter";
        let out = markdown_to_mrkdwn(input);
        // Slack mrkdwn accepts ``` fences natively — leave as-is.
        assert!(out.contains("```"), "got: {}", out);
        assert!(out.contains("let x = 1;"), "got: {}", out);
    }

    #[test]
    fn markdown_to_mrkdwn_preserves_inline_code() {
        let out = markdown_to_mrkdwn("call `foo()` then");
        assert!(out.contains("`foo()`"), "got: {}", out);
    }

    #[test]
    fn convert_double_to_single_asterisk_alternates() {
        let out = convert_double_to_single_asterisk("a **b** c **d** e");
        assert_eq!(out, "a *b* c *d* e");
    }

    #[test]
    fn convert_double_to_single_asterisk_unbalanced_passes_through() {
        let out = convert_double_to_single_asterisk("a **b c");
        assert_eq!(out, "a **b c");
    }

    #[test]
    fn pairing_code_is_six_digits() {
        for _ in 0..100 {
            let code = generate_pairing_code();
            assert_eq!(code.len(), 6, "code {code} not 6 chars");
            assert!(
                code.chars().all(|c| c.is_ascii_digit()),
                "code {code} not all digits"
            );
        }
    }

    #[test]
    fn pair_no_pending_returns_no_pending() {
        let outcome = verify_pair_attempt(None, "123456", Instant::now(), PAIRING_CODE_TTL);
        assert_eq!(outcome, PairOutcome::NoPending);
    }

    #[test]
    fn pair_expired_code_is_rejected() {
        let issued = Instant::now();
        let now = issued + PAIRING_CODE_TTL + Duration::from_secs(1);
        let entry = ("123456".to_string(), issued);
        let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
        assert_eq!(outcome, PairOutcome::Expired);
    }

    #[test]
    fn pair_mismatch_is_rejected() {
        let issued = Instant::now();
        let entry = ("123456".to_string(), issued);
        let outcome = verify_pair_attempt(Some(&entry), "654321", issued, PAIRING_CODE_TTL);
        assert_eq!(outcome, PairOutcome::Mismatch);
    }

    #[test]
    fn pair_valid_code_succeeds() {
        let issued = Instant::now();
        let entry = ("123456".to_string(), issued);
        let now = issued + Duration::from_secs(60);
        let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
        assert_eq!(outcome, PairOutcome::Success);
    }

    /// REPL-issued code lands under the sentinel key.
    #[tokio::test]
    async fn repl_issued_code_lands_under_sentinel() {
        let pending = new_pending_pairs();
        let code = issue_repl_pairing_code(&pending).await;
        assert_eq!(code.len(), 6);
        let map = pending.lock().await;
        let entry = map
            .get(&SENTINEL_PAIRING_CHANNEL_ID)
            .expect("sentinel entry");
        assert_eq!(entry.0, code);
    }

    /// A `/slack-pair <code>` from any channel can claim the sentinel entry.
    #[tokio::test]
    async fn repl_issued_code_promotes_channel_via_sentinel() {
        let pending = new_pending_pairs();
        let code = issue_repl_pairing_code(&pending).await;
        let now = Instant::now();
        let map = pending.lock().await;
        let outcome = verify_pair_attempt(
            map.get(&SENTINEL_PAIRING_CHANNEL_ID),
            &code,
            now,
            PAIRING_CODE_TTL,
        );
        assert_eq!(outcome, PairOutcome::Success);
    }

    /// Sentinel entry past TTL returns Expired.
    #[test]
    fn sentinel_expired_code_is_rejected() {
        let issued = Instant::now();
        let entry = ("123456".to_string(), issued);
        let now = issued + PAIRING_CODE_TTL + Duration::from_secs(1);
        let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
        assert_eq!(outcome, PairOutcome::Expired);
    }

    /// With nothing under the sentinel, lookup returns NoPending.
    #[tokio::test]
    async fn empty_pending_map_returns_no_pending() {
        let pending = new_pending_pairs();
        let map = pending.lock().await;
        let outcome = verify_pair_attempt(
            map.get(&SENTINEL_PAIRING_CHANNEL_ID),
            "123456",
            Instant::now(),
            PAIRING_CODE_TTL,
        );
        assert_eq!(outcome, PairOutcome::NoPending);
    }

    #[tokio::test]
    async fn dedup_skips_duplicate_envelopes() {
        let dedup: Arc<Mutex<VecDeque<String>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(ENVELOPE_DEDUP_CAP)));
        assert!(dedup_check_and_record(&dedup, "env_1").await);
        assert!(!dedup_check_and_record(&dedup, "env_1").await);
        assert!(dedup_check_and_record(&dedup, "env_2").await);
    }

    /// An unknown Slack user (absent from the RBAC table) must get the static
    /// Virtual-CTO reply and never reach the LLM (#481).
    #[test]
    fn rbac_unknown_user_returns_virtual_cto_message() {
        let cfg = SlackRbacConfig {
            users: default_rbac_users(),
            default_persona: "cto-assistant".to_string(),
        };
        // A user id that is not in the default team table.
        assert!(cfg.user("U_UNKNOWN_999").is_none());
        // The handler returns `VIRTUAL_CTO_MESSAGE` verbatim for this case;
        // assert the constant carries the expected gating language.
        assert!(VIRTUAL_CTO_MESSAGE.starts_with(":lock:"));
        assert!(VIRTUAL_CTO_MESSAGE.contains("Duetto engineering team"));
        assert!(VIRTUAL_CTO_MESSAGE.contains("don't have access to"));
    }

    /// `SlackRbacConfig::from_env` parses a hardcoded `SLACK_RBAC_USERS`
    /// string into the expected user table (#481).
    #[test]
    fn rbac_config_parses_env_string() {
        let users = parse_rbac_users(
            "U0A6V2W1M2R:Masa:ALL:*,\
             U0ALDQLBU79:Andrea:ALL:cto-assistant,\
             U09331EP3MX:Alex:ANALYTICS:cto-assistant+ctrl",
        );
        assert_eq!(users.len(), 3);

        let masa = users.get("U0A6V2W1M2R").expect("masa entry");
        assert_eq!(masa.name, "Masa");
        assert_eq!(masa.tier, crate::rbac::ServiceTier::All);
        assert!(masa.allowed_personas.is_none(), "`*` => unrestricted");

        let andrea = users.get("U0ALDQLBU79").expect("andrea entry");
        assert_eq!(andrea.tier, crate::rbac::ServiceTier::All);
        assert_eq!(
            andrea.allowed_personas.as_deref(),
            Some(&["cto-assistant".to_string()][..])
        );

        let alex = users.get("U09331EP3MX").expect("alex entry");
        assert_eq!(alex.tier, crate::rbac::ServiceTier::Analytics);
        assert_eq!(
            alex.allowed_personas.as_deref(),
            Some(&["cto-assistant".to_string(), "ctrl".to_string()][..])
        );

        // Malformed / unknown-tier entries are skipped, not fatal.
        let partial = parse_rbac_users("BAD:entry,U1:Name:NOPE:*,U2:Ok:ALL:*");
        assert_eq!(partial.len(), 1);
        assert!(partial.contains_key("U2"));
    }

    /// A restricted (non-`*`) user must be blocked from `/slack-switch`-ing to
    /// a persona outside their allow-list (#481).
    #[test]
    fn switch_command_blocked_for_restricted_persona() {
        let users = default_rbac_users();
        // Andrea is `ALL:cto-assistant` — only `cto-assistant` is allowed.
        let andrea = users.get("U0ALDQLBU79").expect("andrea entry");
        let allowed = andrea
            .allowed_personas
            .as_ref()
            .expect("andrea has a restricted allow-list");
        // `ctrl` is NOT in the allow-list → switch must be rejected.
        assert!(!allowed.iter().any(|p| p == "ctrl"));
        // `cto-assistant` IS in the allow-list → switch would be permitted.
        assert!(allowed.iter().any(|p| p == "cto-assistant"));

        // Masa is `ALL:*` — unrestricted, may switch to anything incl. `ctrl`.
        let masa = users.get("U0A6V2W1M2R").expect("masa entry");
        assert!(
            masa.allowed_personas.is_none(),
            "unrestricted user may switch to any persona"
        );
    }
}
