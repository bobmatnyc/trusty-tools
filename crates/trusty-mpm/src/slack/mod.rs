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
pub mod focus;
pub mod formatter;
pub mod lifecycle;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::client::{
    ChatMessage, CommandExecutor, CommandResult, FreeTextRoute, ManagedBackend, SessionProxy,
    TrustyCommand, route_free_text,
};
use commands::{SlackCommand, parse_slash};
use focus::ProxyVerb;
use formatter::SlackFormatter;
// Process-lifecycle (PID file + precise stop) lives in its own module to keep
// this file under the 500-SLOC production cap (#2549); re-exported so the
// external `slack::pid_file_path` / `slack::stop_via_pid_file` / `slack::StopOutcome`
// paths the `tm slack` CLI depends on stay stable.
use lifecycle::{PidFileGuard, write_pid_file};
pub use lifecycle::{StopOutcome, pid_file_path, stop_via_pid_file};

/// Ceiling on establishing the TCP connection to `slack.com`.
///
/// Why (#2517): the bot's `reqwest::Client` used to be a bare
/// `reqwest::Client::new()` with no timeout — a stalled Slack API endpoint
/// would hang [`open_socket_url`]/[`post_message`] indefinitely. Mirrors the
/// `CONNECT_TIMEOUT_SECS` precedent in `core::sm::providers::{anthropic,
/// openrouter}` for an external API call over the public internet (wider
/// than the 3s loopback-daemon bound in `client::http_client::config`).
/// What: passed to `reqwest::ClientBuilder::connect_timeout` below.
/// Test: `tests::build_slack_client_bounds_a_stalled_connection`.
const SLACK_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Ceiling on one Slack API request/response round trip.
///
/// Why (#2517): `chat.postMessage` and `apps.connections.open` normally
/// complete in well under a second; 30s is generous slack for a loaded Slack
/// API or a slow network path while still bounding a hung request instead of
/// leaving the bot's message-send/reconnect path unbounded forever.
/// What: passed to `reqwest::ClientBuilder::timeout` below.
/// Test: `tests::build_slack_client_bounds_a_stalled_connection`.
const SLACK_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Build the `reqwest::Client` [`run`] uses for outbound Slack API calls.
///
/// Why: factored out as a pure function of its bounds (mirrors
/// `client::http_client::config::build_client`) so the timeout behavior is
/// unit-testable against tiny durations without waiting out the real 10s/30s
/// production values.
/// What: `reqwest::Client::builder().connect_timeout(..).timeout(..).build()`,
/// falling back to `Client::default()` (unbounded) on the (practically
/// unreachable) builder error, logged at `warn` so a silent regression back
/// to an unbounded client is never silent.
/// Test: `tests::build_slack_client_bounds_a_stalled_connection`.
fn build_slack_client(
    connect_timeout: std::time::Duration,
    request_timeout: std::time::Duration,
) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "slack: reqwest::ClientBuilder::build failed, falling back to an \
                 UNBOUNDED default client (connect_timeout/timeout bounds are NOT applied)"
            );
            reqwest::Client::default()
        })
}

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

/// Initial reconnect backoff after a transient socket drop.
///
/// Why: Slack recycles Socket-Mode connections routinely; the first reconnect
/// should be near-immediate, with the delay growing only if drops persist.
const RECONNECT_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(2);

/// Maximum reconnect backoff (cap for the exponential growth).
///
/// Why: an unbounded exponential would eventually stop retrying for minutes;
/// capping at 60s keeps the bot responsive once Slack recovers while still
/// backing off hard during an outage instead of hammering at 2s forever.
const RECONNECT_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(60);

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
    /// A plain user message (free text): routed to the focused session (proxy
    /// INJECT direction) when the conversation is focused, else to the
    /// action-capable coordinator.
    Message {
        /// Socket-Mode envelope id to ACK.
        envelope_id: String,
        /// The message text.
        text: String,
        /// The channel id the reply should be posted to.
        channel: String,
        /// The thread timestamp when the message is in a thread, else `None`.
        ///
        /// Why (#2549): the session-manager proxy keys focus per Slack
        /// conversation, and a thread is a distinct conversation from its parent
        /// channel — so a focus set in a thread must scope to that thread. The
        /// `thread_ts` (present only for threaded messages) refines the
        /// conversation key via [`focus::conv`].
        thread: Option<String>,
    },
    /// A `disconnect` control envelope: Slack is tearing down this socket.
    ///
    /// Why: Slack signals socket teardown with a `disconnect` envelope whose
    /// `reason` distinguishes a routine recycle (`refresh_requested`, `warning`)
    /// from a PERMANENT failure (`app_deactivated`, an auth/scope revocation). The
    /// adapter must reconnect on the former and STOP on the latter rather than
    /// hammering Slack forever — so the reason has to survive parsing.
    /// What: carries the raw `reason` string (empty when Slack omitted it).
    /// `disconnect` envelopes need no ACK, so this variant carries no id.
    /// Test: `parse_envelope_disconnect_surfaces_reason`.
    Disconnect {
        /// The raw Slack `reason` (e.g. `refresh_requested`, `app_deactivated`).
        reason: String,
    },
    /// An envelope the adapter only needs to ACK (carries the id when present).
    Ignored {
        /// Socket-Mode envelope id to ACK, when the envelope carried one.
        envelope_id: Option<String>,
    },
}

/// Whether a Slack `disconnect`/connection failure is recoverable.
///
/// Why: the reconnect loop must distinguish a transient drop (reconnect with
/// backoff) from a permanent failure (give up — reconnecting can never succeed
/// and only hammers Slack), so the classification is a typed value, not a bool.
/// What: `Transient` → reconnect; `Permanent` → stop the loop and return an error.
/// Test: `classify_disconnect_reason_permanent_vs_transient`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectKind {
    /// A routine/recoverable disconnect — reconnect with backoff.
    Transient,
    /// A fatal disconnect (app deactivated, auth/scope revoked) — stop.
    Permanent,
}

/// Classify a Slack `disconnect` reason (or connection error) as permanent or transient.
///
/// Why: Slack recycles Socket-Mode connections routinely (`refresh_requested`,
/// `warning`, plain socket drops) — those MUST reconnect. But some reasons mean
/// the connection can never be re-established (`app_deactivated`) or that the
/// credentials are dead (`invalid_auth`, `account_inactive`, `token_revoked`,
/// `token_expired`, `not_authed`, `missing_scope`); reconnecting on those is a
/// hot loop against Slack and never recovers. Centralizing the rule keeps the
/// loop's stop/continue decision testable without a live socket.
/// What: returns [`DisconnectKind::Permanent`] for the known fatal reasons
/// (case-insensitive, substring-matched so wrapped errors like
/// `apps.connections.open failed: invalid_auth` still classify), else
/// [`DisconnectKind::Transient`].
/// Test: `classify_disconnect_reason_permanent_vs_transient`.
pub fn classify_disconnect_reason(reason: &str) -> DisconnectKind {
    /// Reasons that mean "reconnecting can never succeed" — STOP the loop.
    const PERMANENT_REASONS: &[&str] = &[
        "app_deactivated",
        "invalid_auth",
        "account_inactive",
        "token_revoked",
        "token_expired",
        "not_authed",
        "missing_scope",
        "no_permission",
    ];
    let needle = reason.to_ascii_lowercase();
    if PERMANENT_REASONS.iter().any(|r| needle.contains(r)) {
        DisconnectKind::Permanent
    } else {
        DisconnectKind::Transient
    }
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
        // A `disconnect` envelope carries a `reason` the loop must classify
        // (permanent → stop, transient → reconnect); it needs no ACK.
        "disconnect" => {
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            SlackEvent::Disconnect { reason }
        }
        // `hello` and anything else only needs an ACK (or none).
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
    // A threaded message carries `thread_ts` (the parent's ts); a top-level
    // message omits it. It refines the proxy conversation key (#2549) so focus
    // set inside a thread scopes to that thread.
    let thread = event
        .get("thread_ts")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string);

    match envelope_id {
        Some(envelope_id)
            if is_message && !is_bot && !text.trim().is_empty() && !channel.is_empty() =>
        {
            SlackEvent::Message {
                envelope_id,
                text: text.to_string(),
                channel: channel.to_string(),
                thread,
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
        // Recover from poisoning: one handler panicking while holding the lock
        // must not crash every subsequent handler. The history is advisory chat
        // context, so the inner guard is safe to reuse.
        let guard = histories.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(channel).cloned().unwrap_or_default()
    };
    match executor
        .client()
        .coordinator_chat(text, &history, true)
        .await
    {
        Ok(Some(outcome)) => {
            {
                // Recover from poisoning (see the read site above): a poisoned
                // history must not crash the handler — reuse the inner guard.
                let mut guard = histories.lock().unwrap_or_else(|e| e.into_inner());
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
/// Why: this is the adapter's routing core — the three session-manager PROXY
/// verbs (`/focus`, `/unfocus`, `/summary`) drive the per-conversation
/// [`SessionProxy`] (TELUI-6, #2549), every other slash command goes through the
/// executor, and free text either INJECTs to the focused session or drives the
/// action-capable coordinator (mirroring the Telegram adapter's `on_message`
/// split). Keeping it one async function keeps the WebSocket loop a thin driver.
/// What: a `/focus`/`/unfocus`/`/summary` slash verb ([`focus::proxy_verb`])
/// routes to the proxy; any other slash command parses to a [`SlackCommand`] and
/// dispatches (unknown verb → free-text drive). A plain message routes via
/// [`route_free_text`] — INJECT to the focused session when the conversation is
/// focused, else the coordinator. Returns `None` for [`SlackEvent::Ignored`].
/// Test: proxy-verb and inject routing by `slash_focus_reaches_proxy_resolve`,
/// `message_when_focused_reaches_proxy_send`, `slash_summary_reaches_proxy_activity`,
/// `slash_unfocus_reaches_proxy`; command dispatch by the `commands` tests; the
/// parse split by `parse_envelope_*`.
async fn reply_for_event(
    executor: &CommandExecutor,
    histories: &ChatHistories,
    proxy: &SessionProxy,
    event: &SlackEvent,
) -> Option<(String, String)> {
    match event {
        SlackEvent::SlashCommand {
            command,
            text,
            channel,
            ..
        } => {
            // A slash command has no thread context; key by channel alone so a
            // `/focus` here and later plain messages in the same channel share
            // one conversation key.
            let conv = focus::conv(channel, None);
            let body = match focus::proxy_verb(command) {
                // The three PROXY verbs drive the per-conversation focus state
                // machine, never the daemon command surface.
                Some(ProxyVerb::Focus) => focus::handle_focus(proxy, &conv, text).await,
                Some(ProxyVerb::Unfocus) => focus::handle_unfocus(proxy, &conv),
                Some(ProxyVerb::Summary) => focus::handle_summary(proxy, &conv).await,
                None => match parse_slash(command, text) {
                    Some(cmd) => dispatch_slash(executor, cmd).await,
                    // Unknown slash verb → treat the whole thing as free text drive.
                    None => {
                        let combined = format!("{command} {text}");
                        action_chat_reply(executor, histories, channel, combined.trim()).await
                    }
                },
            };
            Some((channel.clone(), body))
        }
        SlackEvent::Message {
            text,
            channel,
            thread,
            ..
        } => {
            // #2565 review: a reply INSIDE a thread keys by `channel:ts`, a
            // DIFFERENT key from a channel-level `/focus`; effective_conv falls
            // back to the bare-channel key when the thread key itself has no
            // focus, so a threaded reply still reaches a channel-focused
            // session (thread-specific focus, when set, still wins).
            let conv = focus::effective_conv(proxy, channel, thread.as_deref());
            let has_focus = proxy.current_focus(&conv).is_some();
            let body = match route_free_text(text, has_focus) {
                // A focused conversation injects the line straight to that session.
                FreeTextRoute::Inject => focus::inject_reply(proxy, &conv, text).await,
                // Otherwise natural language drives the fleet via the coordinator.
                FreeTextRoute::Coordinator => {
                    action_chat_reply(executor, histories, channel, text).await
                }
            };
            Some((channel.clone(), body))
        }
        SlackEvent::Disconnect { .. } | SlackEvent::Ignored { .. } => None,
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

/// Why one Socket-Mode connection ended, so [`run`] can decide to reconnect or stop.
///
/// Why: a connection can end three ways — a clean socket drop (reconnect), Slack
/// asking us to recycle via a transient `disconnect` (reconnect), or a permanent
/// `disconnect` reason such as `app_deactivated` (STOP — reconnecting can never
/// succeed). Modelling the outcome as a typed value keeps [`run`]'s loop a thin
/// match instead of string-sniffing an `anyhow::Error`.
/// What: [`Dropped`](SocketOutcome::Dropped) and
/// [`TransientDisconnect`](SocketOutcome::TransientDisconnect) reconnect with
/// backoff; [`PermanentDisconnect`](SocketOutcome::PermanentDisconnect) stops.
/// Test: classification is covered by `classify_disconnect_reason_permanent_vs_transient`.
enum SocketOutcome {
    /// The socket closed/errored (transient); reconnect after backoff.
    Dropped(anyhow::Error),
    /// Slack sent a transient `disconnect` reason; reconnect after backoff.
    TransientDisconnect(String),
    /// Slack sent a permanent `disconnect` reason; stop the loop.
    PermanentDisconnect(String),
}

/// Compute the next reconnect backoff: double, capped at [`RECONNECT_BACKOFF_CAP`].
///
/// Why: capped exponential backoff is the difference between gracefully riding out
/// a Slack outage and a 2s-interval hot loop; extracting the arithmetic makes the
/// cap and doubling unit-testable without sleeping.
/// What: returns `min(current * 2, cap)`. Saturating so a near-`MAX` duration can
/// never overflow.
/// Test: `next_backoff_doubles_and_caps`.
fn next_backoff(current: std::time::Duration, cap: std::time::Duration) -> std::time::Duration {
    current.saturating_mul(2).min(cap)
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
/// post it. A transient socket drop or `disconnect` reconnects with **capped
/// exponential backoff** ([`RECONNECT_BACKOFF_BASE`] → [`RECONNECT_BACKOFF_CAP`],
/// reset to base after a long-lived connection); a **permanent** `disconnect`
/// reason (`app_deactivated`, `invalid_auth`, …) stops the loop and returns an
/// error rather than hammering Slack forever.
/// Test: `--check` mode is deterministic; the backoff arithmetic
/// (`next_backoff_doubles_and_caps`) and reason classification
/// (`classify_disconnect_reason_permanent_vs_transient`) are unit-tested; the live
/// loop is exercised against a real Slack app (deferred — see the PR body).
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

    // #2517: bounded — see [`build_slack_client`].
    let http = build_slack_client(
        std::time::Duration::from_secs(SLACK_CONNECT_TIMEOUT_SECS),
        std::time::Duration::from_secs(SLACK_REQUEST_TIMEOUT_SECS),
    );
    let executor = Arc::new(CommandExecutor::new(url));
    let histories: ChatHistories = Arc::new(Mutex::new(HashMap::new()));

    // The channel-agnostic session-manager proxy (TELUI-6, #2549): holds
    // per-conversation focus state and drives the INJECT/SUMMARIZE directions
    // over the SAME shared executor. Slack is one thin binding; Telegram and the
    // daemon's local proxy routes reuse the identical `SessionProxy`.
    let proxy_backend = Arc::clone(&executor) as Arc<dyn ManagedBackend>;
    let proxy = Arc::new(SessionProxy::new(proxy_backend));

    // Record our PID so `tm slack stop` can signal exactly this process, and
    // hold a RAII guard so the file is removed on EVERY exit path (clean return,
    // early `?`, or panic) — a stale PID file could otherwise misdirect a later
    // `tm slack stop` SIGTERM at a recycled, unrelated PID.
    write_pid_file();
    let _pid_guard = PidFileGuard {
        path: pid_file_path(),
    };

    tracing::info!("Slack adapter starting (Socket Mode)");
    let mut backoff = RECONNECT_BACKOFF_BASE;
    loop {
        // A failure to even open the socket is classified too: a permanent auth
        // error (`invalid_auth`, …) must stop, not hot-loop on `connections.open`.
        let ws_url = match open_socket_url(&http, &app_token).await {
            Ok(url) => url,
            Err(e) => {
                if classify_disconnect_reason(&e.to_string()) == DisconnectKind::Permanent {
                    return Err(e.context("Slack connection permanently failed"));
                }
                tracing::warn!("apps.connections.open failed, retrying: {e}");
                tokio::time::sleep(backoff).await;
                backoff = next_backoff(backoff, RECONNECT_BACKOFF_CAP);
                continue;
            }
        };

        let connected_at = std::time::Instant::now();
        match socket_loop(&http, &bot_token, &ws_url, &executor, &histories, &proxy).await {
            SocketOutcome::PermanentDisconnect(reason) => {
                anyhow::bail!("Slack sent a permanent disconnect ({reason}); stopping");
            }
            SocketOutcome::TransientDisconnect(reason) => {
                tracing::warn!("Slack disconnect ({reason}), reconnecting");
            }
            SocketOutcome::Dropped(e) => {
                tracing::warn!("Slack socket dropped, reconnecting: {e}");
            }
        }

        // Reset backoff after a connection that lived long enough to be healthy,
        // so a single long-lived socket does not inherit an outage's long delay.
        if connected_at.elapsed() >= RECONNECT_BACKOFF_CAP {
            backoff = RECONNECT_BACKOFF_BASE;
        }
        tokio::time::sleep(backoff).await;
        backoff = next_backoff(backoff, RECONNECT_BACKOFF_CAP);
    }
}

/// Drive one Socket-Mode WebSocket connection until it drops.
///
/// Why: separating the per-connection loop from [`run`] keeps the reconnect logic
/// readable and the I/O contained — each frame is parsed, ACKed, and answered.
/// What: connects to `ws_url`, then for each text frame: parses it
/// ([`parse_envelope`]), ACKs the envelope, computes a reply
/// ([`reply_for_event`]), and posts it ([`post_message`]). Returns a
/// [`SocketOutcome`] so [`run`] can reconnect (drop / transient `disconnect`) or
/// stop (permanent `disconnect`). A `disconnect` envelope is classified via
/// [`classify_disconnect_reason`] and ends the connection immediately.
/// Test: classification/backoff are unit-tested; the live socket is deferred.
async fn socket_loop(
    http: &reqwest::Client,
    bot_token: &str,
    ws_url: &str,
    executor: &Arc<CommandExecutor>,
    histories: &ChatHistories,
    proxy: &Arc<SessionProxy>,
) -> SocketOutcome {
    let (ws, _) = match tokio_tungstenite::connect_async(ws_url).await {
        Ok(ws) => ws,
        Err(e) => return SocketOutcome::Dropped(e.into()),
    };
    let (mut write, mut read) = ws.split();

    while let Some(frame) = read.next().await {
        let raw = match frame {
            Ok(WsMessage::Text(raw)) => raw,
            Ok(_) => continue,
            Err(e) => return SocketOutcome::Dropped(e.into()),
        };
        let event = parse_envelope(&raw);
        // A `disconnect` ends this connection; its reason decides stop vs reconnect.
        if let SlackEvent::Disconnect { reason } = &event {
            return match classify_disconnect_reason(reason) {
                DisconnectKind::Permanent => SocketOutcome::PermanentDisconnect(reason.clone()),
                DisconnectKind::Transient => SocketOutcome::TransientDisconnect(reason.clone()),
            };
        }
        // ACK first so Slack does not redeliver while we work.
        if let Some(id) = envelope_id_of(&event)
            && let Err(e) = write.send(WsMessage::Text(ack_frame(&id))).await
        {
            return SocketOutcome::Dropped(e.into());
        }
        if let Some((channel, body)) = reply_for_event(executor, histories, proxy, &event).await {
            post_message(http, bot_token, &channel, &body).await;
        }
    }
    SocketOutcome::Dropped(anyhow::anyhow!("socket closed"))
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
        SlackEvent::Disconnect { .. } => None,
        SlackEvent::Ignored { envelope_id } => envelope_id.clone(),
    }
}

#[cfg(test)]
mod tests;
