//! trusty-mpm Slack bot library — a thin chat-core adapter (DOC-20, #1294).
//!
//! Why: Slack is a peer control surface to the Telegram bot and the sessions
//! TUI (DOC-18 §ONB-3): an operator drives the managed fleet from Slack with the
//! SAME verb set, dispatched through the SAME chat-core nucleus
//! ([`crate::client::CommandExecutor`]). This crate is a *thin adapter* (DOC-20
//! §2): it parses Slack's native input into the shared [`TrustyCommand`], runs it
//! through the one executor, renders the [`CommandResult`] via
//! [`SlackFormatter`], and routes free text to the action-capable coordinator —
//! exactly as the Telegram adapter does. It embeds NO session logic and NO daemon
//! HTTP of its own.
//! What: [`run`] connects to Slack in **Socket Mode** (no public webhook needed),
//! receives slash-command and message events, dispatches them, and posts replies
//! via `chat.postMessage`. [`commands`] holds the native slash-command enum and
//! its projection onto [`TrustyCommand`]; [`formatter`] renders results to Slack
//! `mrkdwn`. The Socket-Mode envelope parsing and the free-text/slash routing
//! decision are pure functions, unit-tested without a live Slack.
//! Test: `cargo test -p trusty-mpm --features slack` covers envelope parsing,
//! command conversion (`commands`), and result formatting (`formatter`). The live
//! WebSocket loop is exercised only against a real Slack app (deferred — needs an
//! installed app; see the PR body).

pub mod commands;
pub mod formatter;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::client::{ChatMessage, CommandExecutor, CommandResult, TrustyCommand};
use commands::{SlackCommand, parse_slash};
use formatter::SlackFormatter;

/// Per-channel coordinator conversation history, keyed by Slack channel id.
///
/// Why: free-text (non-command) messages route to the action-capable
/// coordinator, which is stateless about conversations — the bot holds the
/// rolling history per channel and threads it through each turn (mirrors the
/// Telegram adapter's `ChatHistories`).
/// What: an `Arc<Mutex<…>>` of channel-id → message-history shared across handler
/// tasks.
type ChatHistories = Arc<Mutex<HashMap<String, Vec<ChatMessage>>>>;

/// Maximum number of `ChatMessage`s retained per channel in the rolling history.
///
/// Why: the coordinator is stateless about conversations, so the bot keeps the
/// history client-side and re-sends it every turn. A cap keeps memory flat and
/// the prompt payload bounded on a long-lived channel (mirrors the Telegram
/// adapter's `MAX_CHAT_HISTORY_TURNS`).
/// What: 20 messages = the last 10 user/assistant exchanges.
/// Test: `record_chat_turn_caps_history`.
const MAX_CHAT_HISTORY_TURNS: usize = 20;

/// The reply shown when LLM chat is requested but not configured on the daemon.
const LLM_NOT_CONFIGURED: &str =
    "LLM chat not configured — set OPENROUTER_API_KEY in .env.local and enable the overseer";

/// Slack `apps.connections.open` endpoint (opens a Socket-Mode WebSocket URL).
const CONNECTIONS_OPEN_URL: &str = "https://slack.com/api/apps.connections.open";

/// Slack `chat.postMessage` endpoint (posts a bot reply).
const POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";

/// A parsed Socket-Mode inbound event the adapter knows how to act on.
///
/// Why: Slack's Socket-Mode WebSocket multiplexes several envelope types
/// (`hello`, `disconnect`, `events_api`, `slash_commands`); the adapter only acts
/// on the two that carry operator intent and must always ACK them by
/// `envelope_id`. Modelling the relevant shape as one enum keeps the I/O loop a
/// thin match.
/// What: [`SlashCommand`](SlackEvent::SlashCommand) carries a `/verb` + text +
/// channel; [`Message`](SlackEvent::Message) carries free text + channel; both
/// carry the `envelope_id` to ACK. [`Ignored`](SlackEvent::Ignored) covers
/// envelopes that need only an ACK (or none).
/// Test: `parse_envelope_*` cover each shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlackEvent {
    /// A slash command (`/fleet`, `/status <id>`, …) addressed to the bot.
    SlashCommand {
        /// Socket-Mode envelope id to ACK.
        envelope_id: String,
        /// The command verb WITH its leading slash (e.g. `/fleet`).
        command: String,
        /// The free-form argument tail (may be empty).
        text: String,
        /// The channel id the reply should be posted to.
        channel: String,
    },
    /// A plain user message (free text) routed to the action-capable coordinator.
    Message {
        /// Socket-Mode envelope id to ACK.
        envelope_id: String,
        /// The message text.
        text: String,
        /// The channel id the reply should be posted to.
        channel: String,
    },
    /// An envelope the adapter only needs to ACK (carries the id when present).
    Ignored {
        /// Socket-Mode envelope id to ACK, when the envelope carried one.
        envelope_id: Option<String>,
    },
}

/// Resolve a secret the same way the Telegram adapter and LLM overseer do:
/// `.env.local`, then `.env`, then the process environment.
///
/// Why: operators store Slack tokens in `.env.local` (gitignored) exactly as they
/// store `TELEGRAM_BOT_TOKEN` / `OPENROUTER_API_KEY`; the Slack adapter must honour
/// the same resolution order so one dotenv file configures the whole tool.
/// What: returns the first non-empty value found for `var_name`, or `None`.
/// Test: `resolve_token_reads_dotenv`, `resolve_token_missing_is_none`.
pub fn resolve_token(var_name: &str) -> Option<String> {
    for file in [".env.local", ".env"] {
        if let Some(value) = read_dotenv_key(Path::new(file), var_name) {
            return Some(value);
        }
    }
    std::env::var(var_name).ok().filter(|v| !v.is_empty())
}

/// Read a single `KEY=value` pair from a dotenv-style file.
///
/// Why: pulling the parse out keeps [`resolve_token`] testable against a temp
/// file (mirrors the Telegram adapter's `read_dotenv_key`).
/// What: returns the trimmed, unquoted value for `var_name`, or `None` if the
/// file is absent or the key is not present / empty.
/// Test: `resolve_token_reads_dotenv`.
fn read_dotenv_key(path: &Path, var_name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == var_name
        {
            let value = value.trim().trim_matches('"').trim_matches('\'').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Parse a raw Socket-Mode WebSocket text frame into a [`SlackEvent`].
///
/// Why: this is the adapter's wire-contract boundary — everything downstream is
/// typed. Keeping it a pure function (raw JSON → typed event) makes the routing
/// logic unit-testable without a live Slack socket.
/// What: decodes the envelope, switches on `type`, and extracts the
/// slash-command or message payload plus the `envelope_id` to ACK. An
/// unrecognised / non-actionable envelope becomes [`SlackEvent::Ignored`]
/// carrying any `envelope_id` so the loop still ACKs it. Bot's own messages
/// (those carrying a `bot_id`) are ignored to avoid reply loops.
/// Test: `parse_envelope_slash_command`, `parse_envelope_message`,
/// `parse_envelope_ignores_bot_message`, `parse_envelope_hello_is_ignored`.
pub fn parse_envelope(raw: &str) -> SlackEvent {
    let v: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return SlackEvent::Ignored { envelope_id: None },
    };
    let envelope_id = v
        .get("envelope_id")
        .and_then(|e| e.as_str())
        .map(str::to_string);
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match kind {
        "slash_commands" => {
            let p = &v["payload"];
            let command = p.get("command").and_then(|c| c.as_str()).unwrap_or("");
            let text = p.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let channel = p.get("channel_id").and_then(|c| c.as_str()).unwrap_or("");
            match envelope_id {
                Some(envelope_id) if !command.is_empty() && !channel.is_empty() => {
                    SlackEvent::SlashCommand {
                        envelope_id,
                        command: command.to_string(),
                        text: text.to_string(),
                        channel: channel.to_string(),
                    }
                }
                other => SlackEvent::Ignored { envelope_id: other },
            }
        }
        "events_api" => parse_events_api(&v, envelope_id),
        // `hello`, `disconnect`, and anything else only needs an ACK (or none).
        _ => SlackEvent::Ignored { envelope_id },
    }
}

/// Extract a user message from an `events_api` envelope's inner event.
///
/// Why: free-text drive arrives as an `events_api` envelope wrapping a
/// `message` event; the bot's own posts (which carry a `bot_id`) and non-message
/// events must be filtered so the bot does not answer itself or react to joins.
/// What: returns [`SlackEvent::Message`] for a user `message` with text and a
/// channel; otherwise [`SlackEvent::Ignored`] (still ACKing via `envelope_id`).
/// Test: `parse_envelope_message`, `parse_envelope_ignores_bot_message`.
fn parse_events_api(v: &serde_json::Value, envelope_id: Option<String>) -> SlackEvent {
    let event = &v["payload"]["event"];
    let is_message = event.get("type").and_then(|t| t.as_str()) == Some("message");
    let is_bot = event.get("bot_id").is_some()
        || event.get("subtype").and_then(|s| s.as_str()) == Some("bot_message");
    let text = event.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let channel = event.get("channel").and_then(|c| c.as_str()).unwrap_or("");

    match envelope_id {
        Some(envelope_id)
            if is_message && !is_bot && !text.trim().is_empty() && !channel.is_empty() =>
        {
            SlackEvent::Message {
                envelope_id,
                text: text.to_string(),
                channel: channel.to_string(),
            }
        }
        other => SlackEvent::Ignored { envelope_id: other },
    }
}

/// Build the Socket-Mode ACK frame for an `envelope_id`.
///
/// Why: Slack requires every received envelope to be acknowledged on the socket
/// within 3 seconds or it redelivers; the ACK is a tiny `{ "envelope_id": … }`
/// frame. Factoring it out makes the wire shape testable.
/// What: returns the JSON string `{"envelope_id":"<id>"}`.
/// Test: `ack_frame_shape`.
fn ack_frame(envelope_id: &str) -> String {
    serde_json::json!({ "envelope_id": envelope_id }).to_string()
}

/// Append one conversation turn to a channel's rolling history, bounded.
///
/// Why: the coordinator is stateless, so the bot persists each turn client-side;
/// this centralizes the sliding-window cap ([`MAX_CHAT_HISTORY_TURNS`]) and the
/// skip-empty-reply invariant (mirrors the Telegram adapter's `record_chat_turn`).
/// What: always pushes the user turn; pushes the assistant turn only when `reply`
/// is non-empty; then drains the front so the length never exceeds the cap.
/// Test: `record_chat_turn_caps_history`, `record_chat_turn_skips_empty_reply`.
fn record_chat_turn(entry: &mut Vec<ChatMessage>, text: &str, reply: &str) {
    entry.push(ChatMessage::user(text));
    if !reply.is_empty() {
        entry.push(ChatMessage::assistant(reply));
    }
    if entry.len() > MAX_CHAT_HISTORY_TURNS {
        let overflow = entry.len() - MAX_CHAT_HISTORY_TURNS;
        entry.drain(0..overflow);
    }
}

/// Build a compact "ran: …" footer listing the verbs the coordinator executed.
///
/// Why: when free-text drives the fleet, the operator must see the audit trail of
/// what actually ran — silent side effects are dangerous on a remote surface
/// (mirrors the Telegram adapter's `action_footer`).
/// What: returns `Some("\n\n_ran: a, b_")` (mrkdwn italic, comma-joined) when
/// `actions` is `Some` and non-empty; `None` otherwise.
/// Test: `action_footer_lists_verbs`, `action_footer_absent_when_empty`.
fn action_footer(actions: Option<&[String]>) -> Option<String> {
    let verbs = actions.filter(|v| !v.is_empty())?;
    let joined = verbs.to_vec().join(", ");
    Some(format!("\n\n_ran: {joined}_"))
}

/// Dispatch a parsed slash command through the shared executor and render it.
///
/// Why: a slash command is pure chat-core dispatch — parse to [`TrustyCommand`],
/// run the one executor, render the [`CommandResult`]. Keeping it a free function
/// lets the routing stay a thin match in the loop.
/// What: projects the [`SlackCommand`] onto a [`TrustyCommand`], executes it, and
/// returns the formatted `mrkdwn` body.
/// Test: command conversion is covered by `commands` tests; rendering by
/// `formatter` tests; the executor by its own tests.
async fn dispatch_slash(executor: &CommandExecutor, cmd: SlackCommand) -> String {
    let result: CommandResult = executor.execute(TrustyCommand::from(cmd)).await;
    SlackFormatter::format(&result)
}

/// Route a free-text message to the action-capable coordinator and render it.
///
/// Why: messages that are not slash commands are treated as conversation that can
/// DRIVE the managed fleet — routing them through the coordinator with
/// `actions: true` lets the session-manager invoke managed-session verbs inline
/// (#1283), exactly as the Telegram adapter now does. The bot holds the per-
/// channel history and threads it through `POST /api/v1/sessions/chat`.
/// What: loads the channel's history, calls
/// [`DaemonClient::coordinator_chat`](crate::client::DaemonClient::coordinator_chat)
/// with `actions = true`, persists the turn on success, and returns the reply
/// (plus any captured command output and a `ran: …` footer). A `503` returns the
/// [`LLM_NOT_CONFIGURED`] hint; a transport failure returns an error line.
/// Test: `action_chat_reply_reports_unconfigured` covers the not-configured path;
/// `action_footer_lists_verbs` covers the footer rendering.
async fn action_chat_reply(
    executor: &CommandExecutor,
    histories: &ChatHistories,
    channel: &str,
    text: &str,
) -> String {
    let history = {
        let guard = histories.lock().expect("chat history mutex poisoned");
        guard.get(channel).cloned().unwrap_or_default()
    };
    match executor
        .client()
        .coordinator_chat(text, &history, true)
        .await
    {
        Ok(Some(outcome)) => {
            {
                let mut guard = histories.lock().expect("chat history mutex poisoned");
                let entry = guard.entry(channel.to_string()).or_default();
                record_chat_turn(entry, text, &outcome.reply);
            }
            let mut body = outcome.reply.clone();
            if let Some(output) = outcome.command_output.as_deref()
                && !output.is_empty()
            {
                body.push('\n');
                body.push_str(output);
            }
            if let Some(footer) = action_footer(outcome.actions_taken.as_deref()) {
                body.push_str(&footer);
            }
            body
        }
        Ok(None) => LLM_NOT_CONFIGURED.to_string(),
        Err(e) => format!("❌ chat: daemon error: {e}"),
    }
}

/// Compute the reply body for any actionable [`SlackEvent`].
///
/// Why: this is the adapter's routing core — slash commands go through the
/// executor, free text goes through the action-capable coordinator (mirroring the
/// Telegram adapter's `on_message` split). Keeping it one async function keeps the
/// WebSocket loop a thin driver.
/// What: a slash command parses to a [`SlackCommand`] and dispatches; an unknown
/// slash verb and any plain message route to the coordinator. Returns `None` for
/// [`SlackEvent::Ignored`] (nothing to reply).
/// Test: routing is covered indirectly via `dispatch_slash` / `action_chat_reply`
/// and the `commands` tests; the parse split by `parse_envelope_*`.
async fn reply_for_event(
    executor: &CommandExecutor,
    histories: &ChatHistories,
    event: &SlackEvent,
) -> Option<(String, String)> {
    match event {
        SlackEvent::SlashCommand {
            command,
            text,
            channel,
            ..
        } => {
            let body = match parse_slash(command, text) {
                Some(cmd) => dispatch_slash(executor, cmd).await,
                // Unknown slash verb → treat the whole thing as free text drive.
                None => {
                    let combined = format!("{command} {text}");
                    action_chat_reply(executor, histories, channel, combined.trim()).await
                }
            };
            Some((channel.clone(), body))
        }
        SlackEvent::Message { text, channel, .. } => {
            let body = action_chat_reply(executor, histories, channel, text).await;
            Some((channel.clone(), body))
        }
        SlackEvent::Ignored { .. } => None,
    }
}

/// Open a Socket-Mode WebSocket URL via `apps.connections.open`.
///
/// Why: Socket Mode needs no public webhook — the bot asks Slack for a short-
/// lived `wss://` URL using the app-level token, then connects to it. This is the
/// one HTTP call the adapter makes to Slack's Web API to bootstrap the socket.
/// What: `POST apps.connections.open` with the app token as a bearer; returns the
/// `url` field. Errors when Slack reports `ok: false` or the URL is missing.
/// Test: exercised against a live Slack app (deferred — needs an app token).
async fn open_socket_url(http: &reqwest::Client, app_token: &str) -> anyhow::Result<String> {
    let resp: serde_json::Value = http
        .post(CONNECTIONS_OPEN_URL)
        .bearer_auth(app_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if !resp.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
        let err = resp
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown");
        anyhow::bail!("apps.connections.open failed: {err}");
    }
    resp.get("url")
        .and_then(|u| u.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("apps.connections.open returned no url"))
}

/// Post a `mrkdwn` reply to a Slack channel via `chat.postMessage`.
///
/// Why: replies are posted over the Web API (not the socket) so they appear as
/// normal channel messages; the bot token authorizes the post.
/// What: `POST chat.postMessage` with `{ channel, text, mrkdwn: true }`. Logs (to
/// stderr) and swallows a post failure so one bad reply never tears down the loop.
/// Test: exercised against a live Slack app (deferred).
async fn post_message(http: &reqwest::Client, bot_token: &str, channel: &str, text: &str) {
    let body = serde_json::json!({ "channel": channel, "text": text, "mrkdwn": true });
    match http
        .post(POST_MESSAGE_URL)
        .bearer_auth(bot_token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            if let Ok(v) = resp.json::<serde_json::Value>().await
                && !v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false)
            {
                tracing::warn!(error = ?v.get("error"), "chat.postMessage reported not-ok");
            }
        }
        Err(e) => tracing::warn!("chat.postMessage transport error: {e}"),
    }
}

/// Run the Slack remote-management bot against `url` (the daemon).
///
/// Why: shared entry point for `tm slack start`. Connects to Slack in Socket Mode
/// and drives the managed fleet through the SAME chat-core nucleus as the Telegram
/// bot.
/// What: with `check`, prints the resolved configuration and exits. Otherwise it
/// requires a bot token (`chat.postMessage`) and an app token
/// (`apps.connections.open`), opens a Socket-Mode WebSocket, and loops: parse each
/// inbound frame, ACK it, compute a reply through the executor / coordinator, and
/// post it. A socket error breaks the inner loop and reconnects.
/// Test: `--check` mode is deterministic; the live loop is exercised against a
/// real Slack app (deferred — see the PR body). The pure routing/parse logic is
/// unit-tested.
pub async fn run(
    url: String,
    bot_token: Option<String>,
    app_token: Option<String>,
    check: bool,
) -> anyhow::Result<()> {
    if check {
        println!("trusty-mpm Slack bot configuration:");
        println!("  daemon url       : {url}");
        println!(
            "  bot token        : {}",
            if bot_token.is_some() { "yes" } else { "no" }
        );
        println!(
            "  app token        : {}",
            if app_token.is_some() { "yes" } else { "no" }
        );
        println!("  connection mode  : Socket Mode (no public webhook required)");
        println!();
        println!("{}", crate::client::command::help_text());
        return Ok(());
    }

    let bot_token = bot_token.ok_or_else(|| {
        anyhow::anyhow!("SLACK_BOT_TOKEN is required (or pass --check to validate config)")
    })?;
    let app_token = app_token.ok_or_else(|| {
        anyhow::anyhow!("SLACK_APP_TOKEN is required (or pass --check to validate config)")
    })?;

    let http = reqwest::Client::new();
    let executor = Arc::new(CommandExecutor::new(url));
    let histories: ChatHistories = Arc::new(Mutex::new(HashMap::new()));

    tracing::info!("Slack adapter starting (Socket Mode)");
    loop {
        let ws_url = open_socket_url(&http, &app_token).await?;
        if let Err(e) = socket_loop(&http, &bot_token, &ws_url, &executor, &histories).await {
            tracing::warn!("Slack socket dropped, reconnecting: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Drive one Socket-Mode WebSocket connection until it drops.
///
/// Why: separating the per-connection loop from [`run`] keeps the reconnect logic
/// readable and the I/O contained — each frame is parsed, ACKed, and answered.
/// What: connects to `ws_url`, then for each text frame: parses it
/// ([`parse_envelope`]), ACKs the envelope, computes a reply
/// ([`reply_for_event`]), and posts it ([`post_message`]). Returns `Err` when the
/// socket closes or errors so [`run`] can reconnect.
/// Test: exercised against a live Slack app (deferred).
async fn socket_loop(
    http: &reqwest::Client,
    bot_token: &str,
    ws_url: &str,
    executor: &Arc<CommandExecutor>,
    histories: &ChatHistories,
) -> anyhow::Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(ws_url).await?;
    let (mut write, mut read) = ws.split();

    while let Some(frame) = read.next().await {
        let WsMessage::Text(raw) = frame? else {
            continue;
        };
        let event = parse_envelope(&raw);
        // ACK first so Slack does not redeliver while we work.
        if let Some(id) = envelope_id_of(&event) {
            write.send(WsMessage::Text(ack_frame(&id))).await?;
        }
        if let Some((channel, body)) = reply_for_event(executor, histories, &event).await {
            post_message(http, bot_token, &channel, &body).await;
        }
    }
    anyhow::bail!("socket closed")
}

/// Extract the `envelope_id` to ACK for any [`SlackEvent`].
///
/// Why: every received envelope must be ACKed by id, regardless of whether the
/// adapter acts on it; centralizing the extraction keeps the loop simple.
/// What: returns the id for the actionable variants and the optional id carried
/// by [`SlackEvent::Ignored`].
/// Test: covered by `parse_envelope_*` (which assert the carried ids).
fn envelope_id_of(event: &SlackEvent) -> Option<String> {
    match event {
        SlackEvent::SlashCommand { envelope_id, .. } => Some(envelope_id.clone()),
        SlackEvent::Message { envelope_id, .. } => Some(envelope_id.clone()),
        SlackEvent::Ignored { envelope_id } => envelope_id.clone(),
    }
}

#[cfg(test)]
mod tests;
