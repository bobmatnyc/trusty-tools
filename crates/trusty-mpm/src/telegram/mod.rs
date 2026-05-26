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
pub mod formatter;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::ParseMode;
use teloxide::utils::command::BotCommands;
use tokio_util::sync::CancellationToken;

use crate::client::{ChatMessage, CommandExecutor, CommandResult, TrustyCommand};
use alerts::{AlertConfig, LastSeen};
use commands::TelegramCommand;
use formatter::TelegramFormatter;

/// Per-chat LLM conversation history, keyed by Telegram `chat_id`.
///
/// Why: free-text (non-command) messages route to the daemon's LLM chat, which
/// is stateless about conversations — the bot holds the rolling history per
/// chat and threads it through each turn.
/// What: an `Arc<Mutex<…>>` of chat-id → message-history so every teloxide
/// handler task shares one conversation store.
type ChatHistories = Arc<Mutex<HashMap<i64, Vec<ChatMessage>>>>;

/// The reply shown when LLM chat is requested but not configured.
const LLM_NOT_CONFIGURED: &str =
    "LLM chat not configured — set OPENROUTER_API_KEY in .env.local and enable the overseer";

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

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(on_message))
        .branch(Update::filter_callback_query().endpoint(on_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![executor, opts, histories])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    shutdown.cancel();
    Ok(())
}

/// teloxide message handler: authorize, parse, execute, render, reply.
///
/// Why: the dispatcher branch for text messages — kept thin so all command
/// dispatch stays in the shared [`CommandExecutor`].
/// What: rejects unauthorized users, parses the text into a [`TelegramCommand`]
/// via teloxide, dispatches it (the pairing commands need the message's chat id
/// so they are special-cased), formats the [`CommandResult`], and replies with
/// the appropriate inline keyboard.
/// Test: command conversion is covered by `commands` tests; formatting by
/// `formatter` tests; authorization by `authorization_respects_allowed_user`.
async fn on_message(
    bot: Bot,
    msg: Message,
    executor: Arc<CommandExecutor>,
    options: Arc<BotOptions>,
    histories: ChatHistories,
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

    let command = match TelegramCommand::parse(text, "trusty_mpm_bot") {
        Ok(cmd) => cmd,
        Err(_) => {
            // Not a slash command — route the free text to LLM chat (unless the
            // message is empty, in which case there is nothing to ask).
            if !text.trim().is_empty() {
                let reply = llm_chat_reply(&executor, &histories, msg.chat.id.0, text).await;
                bot.send_message(msg.chat.id, reply)
                    .parse_mode(ParseMode::Html)
                    .await?;
            }
            return Ok(());
        }
    };

    let result = dispatch_command(command, &executor, msg.chat.id.0).await;
    let body = TelegramFormatter::format(&result);
    let mut send = bot
        .send_message(msg.chat.id, body)
        .parse_mode(ParseMode::Html);
    if let Some(keyboard) = TelegramFormatter::keyboard_for(&result) {
        send = send.reply_markup(keyboard);
    }
    send.await?;
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

/// Route a free-text message to the daemon's LLM chat and render the reply.
///
/// Why: messages that are not slash commands are treated as conversation; the
/// bot holds the per-chat history and threads it through `POST /llm/chat`.
/// What: loads this chat's history, calls [`DaemonClient::llm_chat`], stores the
/// updated history on success, and returns the assistant reply (HTML-escaped).
/// When the daemon reports LLM chat is not configured (`503`) it returns the
/// [`LLM_NOT_CONFIGURED`] hint; a transport failure returns an error line.
/// Test: `llm_chat_reply_reports_unconfigured` covers the not-configured path.
async fn llm_chat_reply(
    executor: &CommandExecutor,
    histories: &ChatHistories,
    chat_id: i64,
    text: &str,
) -> String {
    let history = {
        let guard = histories.lock().expect("chat history mutex poisoned");
        guard.get(&chat_id).cloned().unwrap_or_default()
    };
    match executor.client().llm_chat(text, &history).await {
        Ok(Some(outcome)) => {
            histories
                .lock()
                .expect("chat history mutex poisoned")
                .insert(chat_id, outcome.history);
            formatter::html_escape(&outcome.reply)
        }
        Ok(None) => LLM_NOT_CONFIGURED.to_string(),
        Err(e) => format!("❌ chat: daemon error: {e}"),
    }
}

/// teloxide callback-query handler for inline-keyboard buttons.
///
/// Why: the `/sessions`, `/projects`, and `/tmux` lists attach action buttons
/// (`[Status] [Approve] [Deny]`, `[Set Active]`, `[Adopt]`) whose taps arrive
/// as callback queries rather than messages.
/// What: parses the `verb:arg` callback data, runs the matching action through
/// the shared executor (project registration and tmux adoption have their own
/// executor methods), answers the callback to clear the client spinner, and
/// posts the reply.
/// Test: callback dispatch reuses the shared executor, covered by its tests.
async fn on_callback(
    bot: Bot,
    query: CallbackQuery,
    executor: Arc<CommandExecutor>,
    options: Arc<BotOptions>,
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
mod tests {
    use super::*;

    #[test]
    fn resolve_token_reads_dotenv() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "TELEGRAM_BOT_TOKEN=\"123:ABC\"").unwrap();
        let value = read_dotenv_key(&path, "TELEGRAM_BOT_TOKEN");
        assert_eq!(value.as_deref(), Some("123:ABC"));
    }

    #[test]
    fn resolve_token_missing_is_none() {
        let value = read_dotenv_key(Path::new("/no/such/.env"), "TELEGRAM_BOT_TOKEN");
        assert!(value.is_none());
    }

    #[test]
    fn authorization_respects_allowed_user() {
        let unrestricted = BotOptions::default();
        assert!(is_authorized(&unrestricted, Some(7)));
        assert!(is_authorized(&unrestricted, None));

        let restricted = BotOptions {
            allowed_user_id: Some(42),
            alert_chat_id: None,
        };
        assert!(is_authorized(&restricted, Some(42)));
        assert!(!is_authorized(&restricted, Some(99)));
        assert!(!is_authorized(&restricted, None));
    }

    /// Spawn the daemon's real HTTP API on a random loopback port.
    ///
    /// Why: lets the bot's command dispatch be tested against the genuine
    /// daemon routes without a live daemon, tmux, or external network.
    /// What: builds `api::router(DaemonState::shared())`, binds an ephemeral
    /// port, serves it on a background task, and returns the state plus base URL.
    /// Test: used by the `dispatch_*` tests below.
    async fn spawn_test_daemon() -> (std::sync::Arc<crate::daemon::state::DaemonState>, String) {
        use crate::daemon::{api, state::DaemonState};
        use std::future::IntoFuture;
        // Root the daemon's persisted state at a throwaway temp directory so
        // the test never reads (or writes) the operator's real pairing record.
        // `keep` leaks the directory so it outlives the background server.
        let root = tempfile::tempdir().unwrap().keep();
        let state = std::sync::Arc::new(DaemonState::with_root(root));
        let router = api::router(std::sync::Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, router).into_future());
        (state, format!("http://{addr}"))
    }

    #[tokio::test]
    async fn llm_chat_reply_reports_unconfigured() {
        // A default test daemon has no OpenRouter key, so a free-text message
        // gets the not-configured hint rather than a model reply.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        let histories: ChatHistories = Arc::new(Mutex::new(HashMap::new()));
        let reply = llm_chat_reply(&executor, &histories, 42, "hello there").await;
        assert_eq!(reply, LLM_NOT_CONFIGURED);
        // No history is stored when chat is unconfigured.
        assert!(histories.lock().unwrap().get(&42).is_none());
    }

    #[tokio::test]
    async fn dispatch_help_returns_help() {
        let executor = CommandExecutor::new("http://unused");
        let result = dispatch_command(TelegramCommand::Help, &executor, 1).await;
        assert!(matches!(result, CommandResult::Help(_)));
    }

    #[tokio::test]
    async fn dispatch_start_with_no_code_queries_state() {
        // `/start` with no code is a pairing-status query against the daemon.
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        let result = dispatch_command(TelegramCommand::Start(String::new()), &executor, 1).await;
        match result {
            CommandResult::PairState { paired } => assert!(!paired),
            other => panic!("expected PairState, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_start_with_deep_link_code_confirms() {
        // `/start <code>` (the `?start=` deep-link form) confirms the code.
        let (state, url) = spawn_test_daemon().await;
        let code = state.generate_pair_code();
        let executor = CommandExecutor::new(url);
        let result = dispatch_command(TelegramCommand::Start(code), &executor, 555).await;
        match result {
            CommandResult::PairSuccess { chat_info } => assert!(chat_info.contains("555")),
            other => panic!("expected PairSuccess, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_pair_with_bad_code_errors() {
        let (_state, url) = spawn_test_daemon().await;
        let executor = CommandExecutor::new(url);
        let result = dispatch_command(TelegramCommand::Pair("ZZZZZZ".into()), &executor, 1).await;
        assert!(matches!(result, CommandResult::Error(_)));
    }
}
