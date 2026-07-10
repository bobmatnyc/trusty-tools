//! trusty-mpm Telegram bot library.
//!
//! Why: remote management lets an operator drive the daemon from a phone —
//! list sessions, check status, approve a pending permission request, inspect
//! the overseer / tmux, pair the bot to a daemon, and receive push alerts.
//! After the client refactor this crate is a *thin adapter*: all command
//! dispatch and daemon I/O lives in the shared `trusty-mpm-client` crate
//! ([`CommandExecutor`]); this crate only wires teloxide, converts the native
//! [`TelegramCommand`] into the shared [`TrustyCommand`], renders results via
//! [`TelegramFormatter`], runs the push-alert loop, and owns the pairing flow.
//! What: [`run`] boots the teloxide dispatcher; [`commands`] holds the native
//! command enum and its conversion; [`formatter`] renders results; [`alerts`]
//! holds the pure alert-decision core.
//! Test: `cargo test -p trusty-mpm-telegram` covers command conversion, alert
//! formatting, the pure alert-loop core, and result formatting.

pub mod alerts;
pub mod commands;
pub mod focus;
pub mod formatter;
pub mod supervisor;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::ParseMode;
use teloxide::utils::command::BotCommands;
use tokio_util::sync::CancellationToken;

use crate::client::{
    ChatMessage, CommandExecutor, CommandResult, FreeTextRoute, ManagedBackend, SessionProxy,
    TrustyCommand, route_free_text,
};
use alerts::{AlertConfig, LastSeen};
use commands::TelegramCommand;
use formatter::TelegramFormatter;

/// Per-chat coordinator conversation history, keyed by Telegram `chat_id`.
///
/// Why: free-text (non-command) messages route to the action-capable
/// coordinator, which is stateless about conversations — the bot holds the
/// rolling history per chat and threads it through each turn.
/// What: an `Arc<Mutex<…>>` of chat-id → message-history so every teloxide
/// handler task shares one conversation store.
type ChatHistories = Arc<Mutex<HashMap<i64, Vec<ChatMessage>>>>;

/// The reply shown when LLM chat is requested but not configured.
const LLM_NOT_CONFIGURED: &str =
    "LLM chat not configured — set OPENROUTER_API_KEY in .env.local and enable the overseer";

/// Maximum number of `ChatMessage`s retained per chat in the rolling history.
///
/// Why: the coordinator is stateless about conversations, so the bot keeps the
/// history client-side and re-sends it every turn. Without a bound this grows
/// without limit for the life of the process — unbounded memory and an
/// ever-larger prompt payload (cost + latency) on a long-lived chat. Capping to
/// the most recent turns keeps memory flat and the context window relevant.
/// What: 20 messages = the last 10 user/assistant exchanges; after appending a
/// turn the front of the history is drained so its length never exceeds this.
/// Test: `record_chat_turn_caps_history` pushes past the cap and asserts the
/// length is clamped and the oldest messages are dropped.
const MAX_CHAT_HISTORY_TURNS: usize = 20;

/// Environment variable that overrides the Telegram bot's `@username`.
const BOT_USERNAME_ENV: &str = "TELEGRAM_BOT_USERNAME";

/// Default bot `@username` used when [`BOT_USERNAME_ENV`] is unset/empty.
///
/// This is the real, currently-deployed bot. The username drives two things:
/// the `t.me/<username>?start=<code>` pairing deep-link, and teloxide's
/// `/command@<username>` stripping when commands are addressed to the bot in
/// group chats.
const DEFAULT_BOT_USERNAME: &str = "t_sess_bot";

/// Resolve the Telegram bot's `@username` for deep-links and command parsing.
///
/// Why: the bot username is deployment-specific (the real bot is `t_sess_bot`,
/// not the historical hardcoded `trusty_mpm_bot`); operators must be able to
/// point the tool at their own bot without recompiling. It is needed both to
/// build the `t.me/<username>?start=<code>` pairing link and to let teloxide
/// strip a `@<username>` suffix from commands addressed to the bot in groups.
/// What: reads [`BOT_USERNAME_ENV`] and falls back to [`DEFAULT_BOT_USERNAME`]
/// when it is unset or empty (delegates the pure choice to [`resolve_username`]).
/// Test: `resolve_username_*` unit tests cover the pure resolution logic.
pub fn bot_username() -> String {
    resolve_username(std::env::var(BOT_USERNAME_ENV).ok())
}

/// Pure resolution core for [`bot_username`].
///
/// Why: factoring the choice out of the env read keeps it testable without
/// mutating global process environment (which races across parallel tests).
/// What: returns the trimmed `env_val` when it is `Some` and non-empty after
/// trimming; otherwise returns [`DEFAULT_BOT_USERNAME`].
/// Test: `resolve_username_uses_env_when_set`,
/// `resolve_username_falls_back_when_unset`,
/// `resolve_username_falls_back_when_empty`.
fn resolve_username(env_val: Option<String>) -> String {
    match env_val {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => DEFAULT_BOT_USERNAME.to_string(),
    }
}

/// Poll interval for the per-session event push-alert loop.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Poll interval for the overseer push-alert loop.
const OVERSEER_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Optional operator restriction + alert routing for the bot runtime.
///
/// Why: the bot can be locked to a single Telegram user and can push
/// unsolicited alerts to one chat; both are optional CLI-driven settings that
/// must thread through the teloxide handlers.
/// What: holds the allowed user id (when restricted) and the alert chat id.
/// Test: the unauthorized branch is exercised by `is_authorized`.
#[derive(Debug, Clone, Default)]
pub struct BotOptions {
    /// When set, only this Telegram user id may use the bot.
    pub allowed_user_id: Option<i64>,
    /// When set, the chat id push alerts are delivered to.
    pub alert_chat_id: Option<i64>,
}

/// Resolve a secret the same way the LLM overseer does: `.env.local`, then
/// `.env`, then the process environment.
///
/// Why: the operator stores the bot token in `.env.local` (gitignored) exactly
/// as they store `OPENROUTER_API_KEY`; the bot must honour that same resolution
/// order so a single dotenv file configures the whole tool.
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
/// file. Mirrors the daemon's `read_dotenv_key`.
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

/// True if a message from `user_id` may be processed under `options`.
///
/// Why: an optionally-restricted bot must reject every other operator.
/// What: returns true when no restriction is configured, or when the message's
/// user id matches the allowed id.
/// Test: `authorization_respects_allowed_user`.
fn is_authorized(options: &BotOptions, user_id: Option<i64>) -> bool {
    match options.allowed_user_id {
        None => true,
        Some(allowed) => user_id == Some(allowed),
    }
}

/// Run the Telegram remote-management bot against `url`.
///
/// Why: shared entry point for both the `trusty-mpm telegram` subcommand and
/// the backward-compatible `trusty-mpm-telegram` shim binary.
/// What: with `check`, prints the resolved configuration and exits; otherwise
/// registers the generated command menu, spawns the push-alert loop (when an
/// alert chat id is configured), and boots the teloxide dispatcher handling
/// both text messages and inline-keyboard callback queries.
/// Test: `--check` mode is deterministic; live behaviour is exercised by
/// running the bot against a daemon. Command handling is covered by tests.
pub async fn run(
    url: String,
    token: Option<String>,
    check: bool,
    options: BotOptions,
) -> anyhow::Result<()> {
    let alert_config = AlertConfig::recommended();

    if check {
        println!("trusty-mpm Telegram bot configuration:");
        println!("  daemon url        : {url}");
        println!(
            "  token configured  : {}",
            if token.is_some() { "yes" } else { "no" }
        );
        println!("  alert categories  : {:?}", alert_config.categories);
        println!("  memory alerts     : {}", alert_config.memory_alerts);
        println!(
            "  alert chat id     : {}",
            options
                .alert_chat_id
                .map(|i| i.to_string())
                .unwrap_or_else(|| "none".into())
        );
        println!(
            "  allowed user id   : {}",
            options
                .allowed_user_id
                .map(|i| i.to_string())
                .unwrap_or_else(|| "unrestricted".into())
        );
        println!();
        println!("{}", crate::client::command::help_text());
        return Ok(());
    }

    let token = token.ok_or_else(|| {
        anyhow::anyhow!("TELEGRAM_BOT_TOKEN is required (or pass --check to validate config)")
    })?;

    let bot = Bot::new(token);

    // Register the command menu so users see a `/`-command picker in Telegram.
    bot.set_my_commands(TelegramCommand::bot_commands()).await?;

    let shutdown = CancellationToken::new();

    // Spawn the push-alert loop when an alert chat id was configured.
    if let Some(chat_id) = options.alert_chat_id {
        let alert_bot = bot.clone();
        let alert_url = url.clone();
        let alert_cfg = alert_config.clone();
        let token = shutdown.clone();
        tokio::spawn(async move {
            run_alert_loop(alert_bot, ChatId(chat_id), alert_url, alert_cfg, token).await;
        });
    }

    // The one executor every handler shares — all daemon I/O goes through it.
    let executor = Arc::new(CommandExecutor::new(url));
    let opts = Arc::new(options);
    // Per-chat LLM conversation history for free-text messages.
    let histories: ChatHistories = Arc::new(Mutex::new(HashMap::new()));
    // The channel-agnostic session-manager proxy (TELUI-6, #1440): holds per-chat
    // focus state and drives the INJECT/SUMMARIZE directions over the shared
    // executor. Telegram is one thin binding; Slack/MCP reuse the same proxy.
    let proxy_backend = Arc::clone(&executor) as Arc<dyn ManagedBackend>;
    let proxy = Arc::new(SessionProxy::new(proxy_backend));

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(on_message))
        .branch(Update::filter_callback_query().endpoint(on_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![executor, opts, histories, proxy])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    shutdown.cancel();
    Ok(())
}

/// Run the Telegram bot under restart-on-exit supervision (#1499).
///
/// Why: the daemon auto-spawns the bot in the background; a bare spawn that logs
/// once and ends on the first error (a 409 long-poll conflict, a network blip)
/// left the Telegram surface silently dead until a full daemon restart. This
/// wraps [`run`] in [`supervisor::supervise`] so a transient error self-heals
/// with bounded exponential backoff, a permanent misconfiguration backs off and
/// gives up loudly instead of tight-looping, and a daemon shutdown stops the bot
/// promptly without blocking graceful shutdown.
/// What: loops [`run`] via the supervisor, classifying each exit — `Ok(())` is a
/// graceful stop, an error is classified by [`supervisor::classify_error`] into
/// transient vs permanent. The `token`, `options`, and `url` are cloned per
/// attempt so each restart gets a fresh bot. Returns when the bot stops
/// gracefully, the permanent-failure budget is exhausted, or `shutdown` fires.
/// Test: the restart/give-up/cancellation logic is covered by `supervisor::tests`
/// against an injected factory; this thin adapter is exercised by running the
/// daemon.
pub async fn run_supervised(
    url: String,
    token: Option<String>,
    options: BotOptions,
    shutdown: CancellationToken,
) {
    let policy = supervisor::BackoffPolicy::default();
    let factory = || {
        let url = url.clone();
        let token = token.clone();
        let options = options.clone();
        async move {
            match run(url, token, false, options).await {
                Ok(()) => supervisor::BotExit::Graceful,
                Err(e) => {
                    let exit = supervisor::classify_error(&e);
                    tracing::warn!("telegram bot run returned an error: {e:#}");
                    exit
                }
            }
        }
    };
    supervisor::supervise(factory, policy, shutdown).await;
}

/// teloxide message handler: authorize, parse, execute, render, reply.
///
/// Why: the dispatcher branch for text messages — kept thin so all command
/// dispatch stays in the shared [`CommandExecutor`].
/// What: rejects unauthorized users, then routes the text. `/focus`/`/unfocus`
/// are adapter-local focus-state ops (TELUI-6, #1440) handled directly; every
/// other slash command is dispatched via teloxide + the shared executor (pairing
/// commands are special-cased for the chat id). A non-command line routes to the
/// focused session's `managed-send` when the chat is focused, otherwise to the
/// action-capable coordinator ([`focus::route_free_text`]).
/// Test: command conversion is covered by `commands` tests; the focus handlers
/// and routing by `focus::tests`; authorization by
/// `authorization_respects_allowed_user`.
async fn on_message(
    bot: Bot,
    msg: Message,
    executor: Arc<CommandExecutor>,
    options: Arc<BotOptions>,
    histories: ChatHistories,
    proxy: Arc<SessionProxy>,
) -> ResponseResult<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64);
    if !is_authorized(&options, user_id) {
        tracing::warn!(?user_id, "unauthorized Telegram message rejected");
        bot.send_message(
            msg.chat.id,
            "🔒 This bot is restricted to authorized operators.",
        )
        .await?;
        return Ok(());
    }

    let chat_id = msg.chat.id.0;
    match TelegramCommand::parse(text, &bot_username()) {
        // `/focus`/`/unfocus`/`/summary` drive the session-manager PROXY (per-chat
        // focus + INJECT/SUMMARIZE), not the daemon command surface, so they are
        // handled here and never converted to a `TrustyCommand`.
        Ok(TelegramCommand::Focus(arg)) => {
            let reply = focus::handle_focus(&proxy, chat_id, &arg).await;
            bot.send_message(msg.chat.id, reply)
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Ok(TelegramCommand::Unfocus) => {
            let reply = focus::handle_unfocus(&proxy, chat_id);
            bot.send_message(msg.chat.id, reply)
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Ok(TelegramCommand::Summary) => {
            let reply = focus::handle_summary(&proxy, chat_id).await;
            bot.send_message(msg.chat.id, reply)
                .parse_mode(ParseMode::Html)
                .await?;
        }
        // Every other recognized slash command dispatches normally — commands
        // keep working while a session is focused (only free text is captured).
        Ok(command) => {
            let result = dispatch_command(command, &executor, chat_id).await;
            let body = TelegramFormatter::format(&result);
            let mut send = bot
                .send_message(msg.chat.id, body)
                .parse_mode(ParseMode::Html);
            if let Some(keyboard) = TelegramFormatter::keyboard_for(&result) {
                send = send.reply_markup(keyboard);
            }
            send.await?;
        }
        // Not a recognized command — route the free text. An empty message has
        // nothing to route.
        Err(_) => {
            if text.trim().is_empty() {
                return Ok(());
            }
            let has_focus = proxy.current_focus(&focus::conv(chat_id)).is_some();
            let reply = match route_free_text(text, has_focus) {
                // A focused chat injects the line straight to that session (#1440).
                FreeTextRoute::Inject => focus::inject_reply(&proxy, chat_id, text).await,
                // Otherwise natural language DRIVES the fleet via the coordinator
                // (#1283).
                FreeTextRoute::Coordinator => {
                    action_chat_reply(&executor, &histories, chat_id, text).await
                }
            };
            bot.send_message(msg.chat.id, reply)
                .parse_mode(ParseMode::Html)
                .await?;
        }
    }
    Ok(())
}

/// Dispatch one [`TelegramCommand`], threading the chat id for pairing.
///
/// Why: most commands are pure `TrustyCommand` dispatch, but the pairing
/// commands need the Telegram chat id (which is not part of the command model)
/// to confirm a code or honour a `?start=<code>` deep link.
/// What: `/pair <code>` and `/start <code>` route to [`CommandExecutor::pair_confirm`]
/// with `chat_id`; every other command (and the no-code pairing case) is
/// converted to a [`TrustyCommand`] and executed normally.
/// Test: pairing dispatch is covered by the executor tests; conversion by the
/// `commands` tests.
async fn dispatch_command(
    command: TelegramCommand,
    executor: &CommandExecutor,
    chat_id: i64,
) -> CommandResult {
    match &command {
        // `/pair <code>` confirms the code for this chat.
        TelegramCommand::Pair(code) if !code.trim().is_empty() => {
            executor.pair_confirm(code.trim(), chat_id).await
        }
        // `/start <code>` is the deep-link form (`?start=<code>`): confirm it.
        TelegramCommand::Start(code) if !code.trim().is_empty() => {
            executor.pair_confirm(code.trim(), chat_id).await
        }
        // Everything else — including `/pair` and `/start` with no code —
        // converts to the shared command model and runs through the executor.
        _ => executor.execute(TrustyCommand::from(command)).await,
    }
}

/// Route a free-text message to the action-capable coordinator and render it.
///
/// Why: messages that are not slash commands are treated as conversation that
/// can DRIVE the managed fleet — routing them through the coordinator with
/// `actions: true` lets the session-manager invoke managed-session verbs inline
/// (#1283), so "spin up a session for repo X" actually does it rather than
/// merely describing it. The bot holds the per-chat history and threads it
/// through `POST /api/v1/sessions/chat`; the coordinator is stateless about
/// conversations, so the user turn and the assistant reply are appended here.
/// What: loads this chat's history, calls
/// [`DaemonClient::coordinator_chat`](crate::client::DaemonClient::coordinator_chat)
/// with `actions = true`, appends the user/assistant turns to the stored history
/// on success, and returns the HTML-escaped reply. When the coordinator routed a
/// `@prefix:` command its captured pane output is appended; when one or more
/// verbs executed, a compact italic `ran: …` footer is appended so the operator
/// sees what ran. When inference is unconfigured the SM returns a graceful
/// degraded reply as HTTP 200 (not 503, per #1524) which is relayed to the
/// operator; `Ok(None)` covers only the legacy non-SM path that still returns
/// 503. A transport failure returns an error line.
/// Test: `action_chat_reply_reports_unconfigured` covers the not-configured
/// path; `action_footer_lists_verbs` covers the footer rendering.
async fn action_chat_reply(
    executor: &CommandExecutor,
    histories: &ChatHistories,
    chat_id: i64,
    text: &str,
) -> String {
    let history = {
        let guard = histories.lock().expect("chat history mutex poisoned");
        guard.get(&chat_id).cloned().unwrap_or_default()
    };
    match executor
        .client()
        .coordinator_chat(text, &history, true)
        .await
    {
        Ok(Some(outcome)) => {
            // The coordinator keeps no conversation state of its own, so persist
            // this turn (user message + assistant reply) for the next message.
            {
                let mut guard = histories.lock().expect("chat history mutex poisoned");
                let entry = guard.entry(chat_id).or_default();
                record_chat_turn(entry, text, &outcome.reply);
            }
            let mut body = formatter::html_escape(&outcome.reply);
            if let Some(output) = outcome.command_output.as_deref()
                && !output.is_empty()
            {
                body.push('\n');
                body.push_str(&formatter::html_escape(output));
            }
            if let Some(footer) = action_footer(outcome.actions_taken.as_deref()) {
                body.push_str(&footer);
            }
            body
        }
        // Why this branch is still reachable: `Ok(None)` maps from HTTP 503,
        // which the legacy LlmOverseer path returns when the overseer is absent
        // (SM disabled AND no OPENROUTER_API_KEY). The action-loop Degraded path
        // now returns HTTP 200 with an explanatory reply (#1524), so it no longer
        // reaches here — but the legacy fallback can still produce a 503.
        Ok(None) => LLM_NOT_CONFIGURED.to_string(),
        Err(e) => format!("❌ chat: daemon error: {e}"),
    }
}

/// Append one conversation turn to a chat's rolling history, bounded.
///
/// Why: the coordinator is stateless, so the bot persists each turn client-side;
/// this centralizes the two invariants that keep that store healthy — a sliding
/// window cap ([`MAX_CHAT_HISTORY_TURNS`], so history can't grow without bound)
/// and skipping empty assistant replies (an empty turn pollutes the context
/// window and the re-sent prompt with a meaningless message).
/// What: always pushes the user turn; pushes the assistant turn only when `reply`
/// is non-empty; then drains the front of `entry` so its length is at most
/// `MAX_CHAT_HISTORY_TURNS`, dropping the oldest messages first.
/// Test: `record_chat_turn_caps_history` and `record_chat_turn_skips_empty_reply`.
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
/// Why: when free-text drives the fleet, the operator must see the audit trail
/// of what actually ran — silent side effects are dangerous on a remote surface.
/// What: returns `Some("\n\n<i>ran: a, b</i>")` (HTML-escaped, comma-joined) when
/// `actions` is `Some` and non-empty; `None` otherwise (text-only turns add no
/// footer). A `Some(&[])` is treated as "nothing ran" and yields `None`.
/// Test: `action_footer_lists_verbs`, `action_footer_absent_when_empty`.
fn action_footer(actions: Option<&[String]>) -> Option<String> {
    let verbs = actions.filter(|v| !v.is_empty())?;
    let joined = verbs
        .iter()
        .map(|v| formatter::html_escape(v))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("\n\n<i>ran: {joined}</i>"))
}

/// teloxide callback-query handler for inline-keyboard buttons.
///
/// Why: the `/sessions`, `/projects`, and `/tmux` lists attach action buttons
/// (`[Status] [Approve] [Deny]`, `[Set Active]`, `[Adopt]`) whose taps arrive
/// as callback queries rather than messages.
/// What: parses the `verb:arg` callback data, runs the matching action through
/// the shared executor (project registration and tmux adoption have their own
/// executor methods), answers the callback to clear the client spinner, and
/// posts the reply. The `focus:<id>` button (TELUI-6, #1440) sets the chat's
/// focused session so a tap in a list is the "session click" that flips the chat
/// into that session's conversation.
/// Test: callback dispatch reuses the shared executor, covered by its tests; the
/// focus handler by `focus::tests`.
async fn on_callback(
    bot: Bot,
    query: CallbackQuery,
    executor: Arc<CommandExecutor>,
    options: Arc<BotOptions>,
    proxy: Arc<SessionProxy>,
) -> ResponseResult<()> {
    bot.answer_callback_query(query.id.clone()).await?;

    let user_id = Some(query.from.id.0 as i64);
    if !is_authorized(&options, user_id) {
        tracing::warn!(?user_id, "unauthorized Telegram callback rejected");
        return Ok(());
    }

    let Some(data) = query.data.as_deref() else {
        return Ok(());
    };
    let Some(chat_id) = query.message.as_ref().map(|m| m.chat().id) else {
        return Ok(());
    };

    // `[🎯 Focus]` on a session row is the "session click" that focuses it; it
    // drives the proxy focus state, so it is handled before the executor dispatch.
    if let Some(("focus", id)) = data.split_once(':') {
        let reply = focus::handle_focus(&proxy, chat_id.0, id).await;
        bot.send_message(chat_id, reply)
            .parse_mode(ParseMode::Html)
            .await?;
        return Ok(());
    }

    let result = match data.split_once(':') {
        Some(("status", id)) => Some(
            executor
                .execute(TrustyCommand::Status {
                    session_id: id.to_string(),
                })
                .await,
        ),
        Some(("approve", id)) => Some(
            executor
                .execute(TrustyCommand::Approve {
                    session_id: id.to_string(),
                })
                .await,
        ),
        Some(("deny", id)) => Some(
            executor
                .execute(TrustyCommand::Deny {
                    session_id: id.to_string(),
                })
                .await,
        ),
        // `[Adopt]` on an external tmux session in the `/tmux` list.
        Some(("adopt", session)) => Some(
            executor
                .execute(TrustyCommand::Adopt {
                    session: session.to_string(),
                })
                .await,
        ),
        // `[Set Active]` on a discovered project in the `/projects` list.
        // Project registration carries a path, not a `TrustyCommand`, so it
        // routes through the executor's dedicated `register_project` method.
        Some(("setproj", path)) => Some(executor.register_project(path).await),
        _ => None,
    };

    if let Some(result) = result {
        bot.send_message(chat_id, TelegramFormatter::format(&result))
            .parse_mode(ParseMode::Html)
            .await?;
    }
    Ok(())
}

/// The push-alert loop: poll the daemon and forward new events to Telegram.
///
/// Why: an absent operator wants to be interrupted when a session hits a
/// permission prompt, an agent fails, or the overseer blocks something —
/// without having to poll the bot themselves.
/// What: every [`SESSION_POLL_INTERVAL`] it fetches `GET /sessions` and each
/// session's `GET /sessions/{id}/events/poll`, runs [`alerts::check_and_alert`] to
/// find new subscribed events, and sends each as a message to `chat_id`. Every
/// [`OVERSEER_POLL_INTERVAL`] it also checks `GET /overseer` for a block
/// decision. Cancelled cleanly via `shutdown`.
/// Test: the pure decision core is `alerts::check_and_alert`, unit-tested
/// directly; the loop itself is exercised only against a live daemon.
pub async fn run_alert_loop(
    bot: Bot,
    chat_id: ChatId,
    daemon_url: String,
    config: AlertConfig,
    shutdown: CancellationToken,
) {
    let client = reqwest::Client::new();
    let last_seen = Arc::new(Mutex::new(LastSeen::new()));
    let mut session_tick = tokio::time::interval(SESSION_POLL_INTERVAL);
    let mut overseer_tick = tokio::time::interval(OVERSEER_POLL_INTERVAL);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("alert loop shutting down");
                return;
            }
            _ = session_tick.tick() => {
                let alerts = poll_session_alerts(&client, &daemon_url, &config, &last_seen).await;
                for alert in alerts {
                    if let Err(e) = bot.send_message(chat_id, &alert.message).await {
                        tracing::warn!("failed to send alert: {e}");
                    }
                }
            }
            _ = overseer_tick.tick() => {
                if let Some(msg) = poll_overseer_alert(&client, &daemon_url).await
                    && let Err(e) = bot.send_message(chat_id, &msg).await {
                        tracing::warn!("failed to send overseer alert: {e}");
                    }
            }
        }
    }
}

/// One iteration of the per-session event poll.
///
/// Why: separating the I/O from the loop keeps [`run_alert_loop`] readable and
/// lets the pure decision (`check_and_alert`) be tested in isolation.
/// What: fetches the session list and each session's events, then delegates to
/// [`alerts::check_and_alert`] which mutates `last_seen` and returns alerts.
/// Test: the decision logic is covered by `alerts::check_and_alert` tests.
async fn poll_session_alerts(
    client: &reqwest::Client,
    daemon_url: &str,
    config: &AlertConfig,
    last_seen: &Mutex<LastSeen>,
) -> Vec<alerts::PendingAlert> {
    let sessions: Vec<serde_json::Value> =
        match client.get(format!("{daemon_url}/sessions")).send().await {
            Ok(r) => match r.json::<serde_json::Value>().await {
                Ok(b) => b["sessions"].as_array().cloned().unwrap_or_default(),
                Err(_) => return Vec::new(),
            },
            Err(_) => return Vec::new(),
        };

    let mut events_by_session = std::collections::HashMap::new();
    for s in &sessions {
        let Some(id) = s["id"].as_str() else { continue };
        let url = format!("{daemon_url}/sessions/{id}/events/poll");
        if let Ok(r) = client.get(&url).send().await
            && let Ok(body) = r.json::<serde_json::Value>().await
        {
            let events = body["events"].as_array().cloned().unwrap_or_default();
            events_by_session.insert(id.to_string(), events);
        }
    }

    let mut guard = last_seen.lock().expect("last_seen mutex poisoned");
    alerts::check_and_alert(&sessions, &events_by_session, &mut guard, config)
}

/// One iteration of the overseer poll.
///
/// Why: a block decision is rare but critical; the operator should hear about
/// it within [`OVERSEER_POLL_INTERVAL`].
/// What: fetches `GET /overseer`; if the overseer is enabled and reports a
/// blocked session, returns a formatted alert.
/// Test: exercised against a live daemon; the formatter is unit-tested as
/// `alerts::format_overseer_block_alert`.
async fn poll_overseer_alert(client: &reqwest::Client, daemon_url: &str) -> Option<String> {
    let body: serde_json::Value = client
        .get(format!("{daemon_url}/overseer"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let o = &body["overseer"];
    if !o["enabled"].as_bool().unwrap_or(false) {
        return None;
    }
    let blocked = o["blocked_session"].as_str()?;
    Some(alerts::format_overseer_block_alert(blocked))
}

#[cfg(test)]
mod tests;
