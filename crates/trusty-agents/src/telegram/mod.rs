//! Telegram bot gateway to the ctrl orchestrator (#264).
//!
//! Why: Lets users drive trusty-agents from any phone via Telegram, exposing the
//! same `ctrl::run_pm_task_with_history` PM loop that powers the local REPL.
//! Each Telegram chat gets its own `ChatSession` keyed by `ChatId`, so
//! conversations from different humans don't trample each other's history.
//!
//! What: Long-polling teloxide bot with `/start`, `/help`, `/connect`,
//! `/clear`, `/status` slash commands plus a plain-text fallback that
//! dispatches to `ctrl`. Responses are sent as `ParseMode::Html` with
//! HTML-escaped content, split at 4096-char boundaries on newline preference.
//!
//! Since #7427 PR 2 this loop is also the Telegram INBOUND SOURCE for the
//! per-assistant channel bindings: a plain-text update whose chat id a saved
//! binding names goes to `agent_channels::inbound::receive_inbound`, the same dispatch
//! Slack's intake uses, and the `ChatSession` path above handles only the
//! updates no binding claims. The bot token comes from the credential authority
//! under the binding's own reference, not from a direct environment read.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — lifecycle (`run_telegram_bot`), session types, dptree wiring
//! - `inbound.rs` — update → `StoredEvent` → `receive_inbound` (#7427)
//! - `pairing.rs` — pairing state machine, persistence, codes, PID guard
//! - `handlers.rs` — `Command` enum + slash/plain-text handlers
//! - `format.rs` — Markdown→HTML conversion + 4096-char chunking
//! - `tests.rs` — unit tests for the pure helpers above
//!
//! Test: Build with `cargo build` (no live token needed), unit-test
//! `split_message` and `markdown_to_html_safe` directly. Live verification is
//! out-of-scope per the issue — this module is wired behind `--telegram`.

// #8190: one bot token, the assistants it may wake, and the key its per-bot
// state files are named after.
mod bot;
mod format;
mod handlers;
// #7427: the bridge from a long-poll update into the per-assistant channel
// bindings. The gateway below is now the Telegram inbound SOURCE; the dispatch
// it feeds is `agent_channels::inbound::receive_inbound`, the same one Slack uses.
mod inbound;
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

use handlers::{BotOwners, Command, handle_command, handle_message, handle_plain_text};
use pairing::{PairedChats, TelegramPidGuard, load_paired_chats, paired_chats_state_path_for};

// Re-export the public REPL-facing API so callers continue to use
// `crate::telegram::{PendingPairs, new_pending_pairs, issue_repl_pairing_code,
// run_telegram_bot}` after the split.
pub use pairing::{
    PendingPairs, SENTINEL_PAIRING_CHAT_ID, issue_repl_pairing_code, new_pending_pairs,
};
// #8190: the API host asks whether a poller already owns `getUpdates` for a
// given bot before it spawns one of its own.
pub(crate) use bot::{BotKey, TelegramBot};
pub use pairing::{LockHolder, gateway_lock_holder_at};
pub(crate) use pairing::{
    gateway_state_dir, live_gateway_lock_holders_in, telegram_pid_file_path_for,
};

/// Take one bot's gateway lock, for a test that needs a live holder.
///
/// Why (#8190): the status snapshot's live-lock probe has no other way to be
/// driven — the real acquirer is inside the poll loop, which needs a network.
/// `cfg(test)` keeps it out of the shipped surface entirely.
/// Test: `telegram_gateway_status_snapshot_reports_a_live_lock_holder`.
#[cfg(test)]
pub(crate) fn acquire_gateway_lock_for_test(path: PathBuf) -> Result<TelegramPidGuard> {
    TelegramPidGuard::acquire(path)
}

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

/// Whether a persona resolves under `<agents_dir>` — as a flat `<name>.toml`
/// OR a directory package `<name>/agent.toml`.
///
/// Why: the `/switch` pre-check must mirror the real dispatch resolver
/// `AgentConfig::by_name_async` (`agents/loader.rs`), which resolves a
/// directory package (`load_agent_package`: `<name>/agent.toml`) BEFORE the
/// flat `<name>.toml`. The canonical `assistant` persona ships ONLY as a
/// directory package (`agents/assistant/agent.toml` + `persona.md`, no flat
/// `assistant.toml`), so the old flat-only pre-check rejected `/switch
/// assistant` with "Unknown persona" even though dispatch resolves it fine.
/// What: returns true iff either a flat `<name>.toml` exists, OR a COMPLETE
/// directory package exists. `load_agent_package` reads `<name>/agent.toml`
/// AND `<name>/persona.md` UNCONDITIONALLY (no `Ok(None)` fallback if the
/// persona is missing), so the package branch requires BOTH — otherwise an
/// incomplete package (agent.toml only) would pass the pre-check here and then
/// hard-fail on the next turn's load, the exact failure this pre-check exists
/// to prevent.
/// Test: `persona_exists_in_accepts_directory_package`,
/// `persona_exists_in_rejects_unknown_name`,
/// `persona_exists_in_rejects_package_missing_persona_md`.
fn persona_exists_in(agents_dir: &std::path::Path, name: &str) -> bool {
    if agents_dir.join(format!("{name}.toml")).exists() {
        return true;
    }
    let pkg = agents_dir.join(name);
    pkg.join("agent.toml").exists() && pkg.join("persona.md").exists()
}

/// Whether a persona resolves under a project's `.trusty-agents/agents/` dir,
/// in either the flat or directory-package form (see [`persona_exists_in`]).
///
/// Why (#457): the project-local tier of the `/switch` pre-check — checked
/// before the `$HOME` fallback, mirroring ctrl's resolution order.
/// What: delegates to [`persona_exists_in`] rooted at
/// `<project_path>/.trusty-agents/agents/`.
/// Test: `project_persona_exists_accepts_directory_package`,
/// `project_persona_exists_rejects_unknown_name`.
pub(super) fn project_persona_exists(project_path: &std::path::Path, name: &str) -> bool {
    persona_exists_in(&project_path.join(".trusty-agents").join("agents"), name)
}

/// Check whether a persona exists under the user's home config dir.
///
/// Why (#457): `/switch <name>` must validate that the persona actually
/// resolves before storing it on the session — otherwise the next turn
/// would fail in `run_pm_task_with_persona` with a load error. We check
/// `~/.trusty-agents/agents/` as a fallback after the project-local path so
/// user-level persona definitions also work.
/// What: Returns `true` iff `$HOME/.trusty-agents/agents/<name>` resolves as a
/// flat `<name>.toml` or a directory package `<name>/agent.toml` (see
/// [`persona_exists_in`]).
/// Test: the directory-package acceptance shared with the project tier is
/// covered by `persona_exists_in_accepts_directory_package`.
pub(super) fn home_persona_exists(name: &str) -> bool {
    std::env::var("HOME")
        .ok()
        .map(|h| {
            persona_exists_in(
                &std::path::PathBuf::from(h)
                    .join(".trusty-agents")
                    .join("agents"),
                name,
            )
        })
        .unwrap_or(false)
}

/// Run the Telegram bot in long-polling mode until SIGINT.
///
/// Why: the entry point wired to `--telegram` in `main.rs` and to the REPL's
/// auto-start. Neither knows which assistant owns which bot, so both poll the
/// host's default Telegram credential and let any assistant's binding claim a
/// chat — the pre-#8190 behaviour, preserved.
/// What: resolves the credential reference the first enabled receiving binding
/// names (#7427), then delegates to [`run_telegram_bot_for`]. A token that will
/// not resolve fails startup rather than polling without one.
/// Test: `telegram_poll_token_refuses_a_credential_outside_the_family`,
/// `agent_channels_poll_credential_ref_reads_the_first_receiving_binding`.
pub async fn run_telegram_bot(project_path: PathBuf, pending: PendingPairs) -> Result<()> {
    let credential_ref = crate::api::server::agent_channels::telegram_poll_credential_ref().await;
    let token = crate::channels::telegram_poll_token(credential_ref.as_deref()).map_err(|e| {
        error!(error = %e, "Telegram bot token could not be resolved; refusing to poll without one");
        anyhow!(
            "Telegram bot token could not be resolved ({e}). Set TELEGRAM_BOT_TOKEN in the \
             harness environment, or point the binding's credential reference at a stored \
             Telegram credential."
        )
    })?;
    // `None` owners: this path predates per-assistant bots and keeps its
    // any-assistant-may-claim dispatch (#8190).
    let bot = TelegramBot::new(token, None, credential_ref.into_iter().collect());
    run_telegram_bot_for(bot, project_path, pending).await
}

/// Run ONE bot's long-poll loop until shutdown.
///
/// Why (#8190, owner ruling 2026-09-16): each assistant has its own Telegram
/// bot, so the loop can no longer resolve a machine-wide token and deliver to
/// whoever matches — a message on izzie's bot must never wake an assistant
/// bound only to cto-assistant's. The bot is now a PARAMETER: its token
/// authenticates `getUpdates`, its owners scope the dispatch, and its key names
/// the lock and pairing files so two bots never share either.
/// What: takes this bot's own `flock` gateway lock, builds a `Bot` with
/// explicit HTTP timeouts, wires `dptree` routes for commands and plain text,
/// then dispatches with Ctrl-C handling enabled.
/// Test: `telegram_pairing_state_is_per_bot`,
/// `telegram_bot_carries_its_owners_and_never_renders_its_key`,
/// `telegram_pid_guard_live_conflict_is_rejected`.
pub(crate) async fn run_telegram_bot_for(
    bot_identity: TelegramBot,
    project_path: PathBuf,
    pending: PendingPairs,
) -> Result<()> {
    // Single-instance guard, per bot: two processes polling `getUpdates` for
    // the SAME bot trigger `TerminatedByOtherGetUpdates`. The guard holds an
    // advisory `flock`, which the kernel releases when the descriptor closes on
    // any exit path — normal return, `?`, panic, SIGINT, process death (#8190).
    let _pid_guard = TelegramPidGuard::acquire(telegram_pid_file_path_for(bot_identity.key()))
        .map_err(|e| {
            error!(bot = %bot_identity.label(), "{e}");
            e
        })?;
    info!(
        bot = %bot_identity.label(),
        "Telegram daemon starting (PID {})",
        std::process::id()
    );
    let paired_state_path = paired_chats_state_path_for(bot_identity.key());
    // #8190: the assistants this bot may wake, carried into every dispatch.
    let owners: BotOwners = bot_identity.owners().map(|o| Arc::new(o.to_vec()));
    let token = bot_identity.into_token();

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

    let bot = Bot::with_client(token.into_inner(), client);

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
    // #8190: from THIS bot's own file — a chat paired to izzie's bot is not
    // paired on cto-assistant's.
    let paired: PairedChats = load_paired_chats(&paired_state_path).await;
    let paired_state_path = Arc::new(paired_state_path);
    // #334: `pending` is supplied by the caller (the REPL) so the REPL's
    // `/telegram pair` command can write codes the bot validates here.

    info!(
        project = %project_path.display(),
        "Starting Telegram bot in long-polling mode"
    );

    // #4703: resolve the attendance root ONCE at startup, then inject it into
    // every handler branch below. Both `handle_command` and `handle_message`
    // used to resolve it inline, which made their attendance hooks impossible
    // to assert on from a test.
    let attendance_root: crate::attendance::AttendanceRoot =
        crate::attendance::default_attendance_root()
            .ok()
            .map(Arc::new);

    let sessions_for_cmd = Arc::clone(&sessions);
    let project_for_cmd = Arc::clone(&project_path);
    let paired_for_cmd = Arc::clone(&paired);
    let paired_path_for_cmd = Arc::clone(&paired_state_path);
    let pending_for_cmd = Arc::clone(&pending);
    let sessions_for_slash = Arc::clone(&sessions);
    let project_for_slash = Arc::clone(&project_path);
    let paired_for_slash = Arc::clone(&paired);
    let owners_for_slash = owners.clone();
    let attendance_for_cmd = attendance_root.clone();
    let attendance_for_slash = attendance_root.clone();
    let attendance_for_msg = attendance_root.clone();
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
                    let attendance_root = attendance_for_cmd.clone();
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
                            attendance_root,
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
                    let attendance_root = attendance_for_slash.clone();
                    // #8190: `/switch` is gateway control, and on a supervised
                    // bot it may not reach an assistant that owns no binding
                    // here.
                    let owners = owners_for_slash.clone();
                    async move {
                        handle_message(bot, msg, sessions, project, paired, attendance_root, owners)
                            .await
                    }
                }),
        )
        // #7427: plain text is the branch a binding can claim. A chat id a
        // saved binding names dispatches through `receive_inbound` — the same
        // path, prompt and failure counter Slack uses. Everything else falls
        // through to the pre-#7427 gateway session, so a host with no bindings
        // behaves exactly as it did — but a SUPERVISED per-assistant bot has no
        // such session and drops the update instead (#8190). Slash commands
        // never route here: they are gateway control (`/pair`, `/connect`,
        // `/switch`), not assistant work.
        .branch(
            Update::filter_message()
                .filter(|msg: Message| msg.text().map(|t| !t.starts_with('/')).unwrap_or(false))
                .endpoint(move |bot: Bot, msg: Message| {
                    let sessions = Arc::clone(&sessions_for_msg);
                    let project = Arc::clone(&project_for_msg);
                    let paired = Arc::clone(&paired_for_msg);
                    let attendance_root = attendance_for_msg.clone();
                    // #8190: only THIS bot's owners may claim the update.
                    let owners = owners.clone();
                    async move {
                        handle_plain_text(
                            bot,
                            msg,
                            sessions,
                            project,
                            paired,
                            attendance_root,
                            owners,
                        )
                        .await
                    }
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
