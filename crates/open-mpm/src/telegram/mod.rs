//! Telegram bot gateway to the ctrl orchestrator (#264).
//!
//! Why: Lets users drive open-mpm from any phone via Telegram, exposing the
//! same `ctrl::run_pm_task_with_history` PM loop that powers the local REPL.
//! Each Telegram chat gets its own `ChatSession` keyed by `ChatId`, so
//! conversations from different humans don't trample each other's history.
//!
//! What: Long-polling teloxide bot with `/start`, `/help`, `/connect`,
//! `/clear`, `/status` slash commands plus a plain-text fallback that
//! dispatches to `ctrl`. Responses are sent as `ParseMode::Html` with
//! HTML-escaped content, split at 4096-char boundaries on newline preference.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — lifecycle (`run_telegram_bot`), session types, dptree wiring
//! - `pairing.rs` — pairing state machine, persistence, codes, PID guard
//! - `handlers.rs` — `Command` enum + slash/plain-text handlers
//! - `format.rs` — Markdown→HTML conversion + 4096-char chunking
//! - `tests.rs` — unit tests for the pure helpers above
//!
//! Test: Build with `cargo build` (no live token needed), unit-test
//! `split_message` and `markdown_to_html_safe` directly. Live verification is
//! out-of-scope per the issue — this module is wired behind `--telegram`.

mod format;
mod handlers;
mod pairing;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use teloxide::dispatching::UpdateFilterExt;
use teloxide::prelude::*;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::ctrl::ConversationTurn;

use handlers::{Command, handle_command, handle_message};
use pairing::{
    PairedChats, TelegramPidGuard, load_paired_chats, paired_chats_state_path,
    telegram_pid_file_path,
};

// Re-export the public REPL-facing API so callers continue to use
// `crate::telegram::{PendingPairs, new_pending_pairs, issue_repl_pairing_code,
// run_telegram_bot}` after the split.
pub use pairing::{
    PendingPairs, SENTINEL_PAIRING_CHAT_ID, issue_repl_pairing_code, new_pending_pairs,
};

/// Maximum characters per Telegram message.
///
/// Why: Telegram's hard cap is 4096 chars per message. Long ctrl responses are
/// split on the last newline before this boundary so we never cut mid-line.
pub(super) const MAX_TELEGRAM_MESSAGE: usize = 4096;

/// HTTP read timeout for `getUpdates`.
///
/// Why: Long-polling holds the connection open. Telegram recommends >= the
/// poll timeout (default 10s); 120s gives generous headroom and matches the
/// reference implementation.
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// HTTP connect timeout.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-chat conversation state.
///
/// Why: Each Telegram chat is a separate conversation with the ctrl PM. We
/// keep history per-chat so /clear in one chat doesn't wipe another, and
/// /connect can rebind a single chat to a different project.
/// What: Tracks the active project path (defaults to the launch path) and
/// the rolling list of `ConversationTurn`s passed to ctrl on each turn.
/// Test: Covered indirectly via `handle_message` exercising the SessionMap.
pub(super) struct ChatSession {
    pub(super) project_path: PathBuf,
    pub(super) history: Vec<ConversationTurn>,
    /// Active persona for this chat (#457).
    ///
    /// Why: `/switch <persona>` must persist across turns so subsequent
    /// messages route through `run_pm_task_with_persona` instead of the
    /// default ctrl/PM agent. `None` means "use the default ctrl runner".
    pub(super) active_persona: Option<String>,
}

impl ChatSession {
    pub(super) fn new(project_path: PathBuf) -> Self {
        Self {
            project_path,
            history: Vec::new(),
            active_persona: None,
        }
    }
}

/// Map of `ChatId` -> per-chat session, shared across handlers.
pub(super) type SessionMap = Arc<Mutex<HashMap<ChatId, ChatSession>>>;

/// Check whether a persona TOML exists under the user's home config dir.
///
/// Why (#457): `/switch <name>` must validate that the persona actually
/// resolves before storing it on the session — otherwise the next turn
/// would fail in `run_pm_task_with_persona` with a load error. We check
/// `~/.open-mpm/agents/<name>.toml` as a fallback after the project-local
/// path so user-level persona definitions also work.
/// What: Returns `true` iff `$HOME/.open-mpm/agents/<name>.toml` is a file.
/// Test: Indirectly via the `/switch` handler.
pub(super) fn home_persona_exists(name: &str) -> bool {
    std::env::var("HOME")
        .ok()
        .map(|h| {
            std::path::PathBuf::from(h)
                .join(".open-mpm")
                .join("agents")
                .join(format!("{name}.toml"))
                .exists()
        })
        .unwrap_or(false)
}

/// Run the Telegram bot in long-polling mode until SIGINT.
///
/// Why: This is the entry point wired to `--telegram` in `main.rs`. Long
/// polling avoids webhook setup (no public URL / TLS termination required) so
/// the bot can run from a developer's laptop or a CI runner identically.
/// What: Loads `TELEGRAM_BOT_TOKEN`, builds a `Bot` with explicit HTTP
/// timeouts (matches the reference implementation), wires `dptree` routes for
/// commands and plain text, then dispatches with Ctrl-C handling enabled.
/// Test: `cargo build` is the primary gate; we never actually contact
/// Telegram in CI.
pub async fn run_telegram_bot(project_path: PathBuf, pending: PendingPairs) -> Result<()> {
    // Single-instance guard: refuse to start if another Telegram daemon is
    // already long-polling, which would otherwise cause Telegram's
    // `TerminatedByOtherGetUpdates` errors. The guard's `Drop` removes the
    // PID file on every exit path (normal return, `?`, SIGINT via the
    // dispatcher's `enable_ctrlc_handler`). Held for the whole function body.
    let _pid_guard = TelegramPidGuard::acquire(telegram_pid_file_path()).map_err(|e| {
        error!("{e}");
        e
    })?;
    info!("Telegram daemon starting (PID {})", std::process::id());

    let token = std::env::var("TELEGRAM_BOT_TOKEN").map_err(|_| {
        anyhow!(
            "TELEGRAM_BOT_TOKEN not set. Add it to .env.local or export it before running --telegram."
        )
    })?;

    // Why: Default reqwest client has aggressive idle timeouts that drop
    // long-poll connections. We mirror the reference bot's settings so
    // getUpdates stays alive between polls.
    let client = teloxide::net::default_reqwest_settings()
        .timeout(HTTP_READ_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(2)
        .build()
        .map_err(|e| anyhow!("failed to build telegram HTTP client: {}", e))?;

    let bot = Bot::with_client(token, client);

    // #333: Startup diagnostics. Long-polling silently drops updates if a
    // webhook is registered, and an invalid token gives a misleading "no
    // response" symptom rather than an error. Verify connectivity and clear
    // any stale webhook *before* dispatching, and surface a clear log line
    // confirming the bot is live.
    let me = match bot.get_me().await {
        Ok(me) => me,
        Err(e) => {
            error!(error = %e, "Telegram getMe failed. Check TELEGRAM_BOT_TOKEN in .env.local");
            return Err(anyhow!(
                "Telegram getMe failed: {e}. Check TELEGRAM_BOT_TOKEN in .env.local"
            ));
        }
    };
    let bot_username = me
        .username
        .clone()
        .unwrap_or_else(|| "<no-username>".to_string());

    match bot.get_webhook_info().await {
        Ok(info) => {
            let url = info.url.as_ref().map(|u| u.as_str()).unwrap_or("");
            if !url.is_empty() {
                warn!(
                    "Active webhook detected: {}. Deleting it to enable long-polling.",
                    url
                );
                if let Err(e) = bot.delete_webhook().await {
                    error!(error = %e, "Failed to delete existing webhook; long-polling may not receive updates");
                    return Err(anyhow!("Failed to delete webhook: {e}"));
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "getWebhookInfo failed; continuing anyway");
        }
    }

    info!(
        "Telegram bot @{} started. Long-polling active.",
        bot_username
    );

    // Resolve and pin the launch project path. Each chat starts from this
    // path; users can rebind via /connect.
    let project_path = Arc::new(project_path);
    let sessions: SessionMap = Arc::new(Mutex::new(HashMap::new()));
    // #467: Load persisted pairings so users don't lose pairing on restart.
    let paired_state_path = paired_chats_state_path();
    let paired: PairedChats = load_paired_chats(&paired_state_path).await;
    let paired_state_path = Arc::new(paired_state_path);
    // #334: `pending` is supplied by the caller (the REPL) so the REPL's
    // `/telegram pair` command can write codes the bot validates here.

    info!(
        project = %project_path.display(),
        "Starting Telegram bot in long-polling mode"
    );

    let sessions_for_cmd = Arc::clone(&sessions);
    let project_for_cmd = Arc::clone(&project_path);
    let paired_for_cmd = Arc::clone(&paired);
    let paired_path_for_cmd = Arc::clone(&paired_state_path);
    let pending_for_cmd = Arc::clone(&pending);
    let sessions_for_slash = Arc::clone(&sessions);
    let project_for_slash = Arc::clone(&project_path);
    let paired_for_slash = Arc::clone(&paired);
    let sessions_for_msg = Arc::clone(&sessions);
    let project_for_msg = Arc::clone(&project_path);
    let paired_for_msg = Arc::clone(&paired);

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(move |bot: Bot, msg: Message, cmd: Command| {
                    let sessions = Arc::clone(&sessions_for_cmd);
                    let project = Arc::clone(&project_for_cmd);
                    let paired = Arc::clone(&paired_for_cmd);
                    let paired_path = Arc::clone(&paired_path_for_cmd);
                    let pending = Arc::clone(&pending_for_cmd);
                    async move {
                        handle_command(
                            bot,
                            msg,
                            cmd,
                            sessions,
                            project,
                            paired,
                            paired_path,
                            pending,
                        )
                        .await
                    }
                }),
        )
        // #457: Catch-all for slash commands not in the `Command` enum
        // (e.g. /switch, /cost, /model). Without this branch they fall
        // through to default_handler and are silently dropped. Forwarding
        // to handle_message routes them through ctrl's try_handle_slash
        // dispatch, which already knows how to handle REPL slash commands.
        // Order matters: this MUST come after filter_command (so known
        // commands keep their dedicated handlers) and before the plain-text
        // branch (which excludes '/'-prefixed messages).
        .branch(
            Update::filter_message()
                .filter(|msg: Message| msg.text().map(|t| t.starts_with('/')).unwrap_or(false))
                .endpoint(move |bot: Bot, msg: Message| {
                    let sessions = Arc::clone(&sessions_for_slash);
                    let project = Arc::clone(&project_for_slash);
                    let paired = Arc::clone(&paired_for_slash);
                    async move { handle_message(bot, msg, sessions, project, paired).await }
                }),
        )
        .branch(
            Update::filter_message()
                .filter(|msg: Message| msg.text().map(|t| !t.starts_with('/')).unwrap_or(false))
                .endpoint(move |bot: Bot, msg: Message| {
                    let sessions = Arc::clone(&sessions_for_msg);
                    let project = Arc::clone(&project_for_msg);
                    let paired = Arc::clone(&paired_for_msg);
                    async move { handle_message(bot, msg, sessions, project, paired).await }
                }),
        );

    Dispatcher::builder(bot, handler)
        .default_handler(|upd| async move {
            tracing::debug!(?upd, "telegram: unhandled update");
        })
        .error_handler(
            teloxide::error_handlers::LoggingErrorHandler::with_custom_text(
                "telegram dispatcher error",
            ),
        )
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}
