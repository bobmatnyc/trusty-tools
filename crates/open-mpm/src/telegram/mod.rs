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
//! Test: Build with `cargo build` (no live token needed), unit-test
//! `split_message` and `markdown_to_html_safe` directly. Live verification is
//! out-of-scope per the issue — this module is wired behind `--telegram`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use teloxide::dispatching::UpdateFilterExt;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, ChatId, MessageId, ParseMode, ReplyParameters};
use teloxide::utils::command::BotCommands;
use teloxide::utils::html;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

use crate::ctrl::{self, ConversationTurn};

/// How long a pairing code remains valid after `/start`.
///
/// Why: Bound the window where a leaked code from server logs could be used by
/// another chat. 5 minutes is long enough for a human to copy/paste and short
/// enough to limit exposure.
const PAIRING_CODE_TTL: Duration = Duration::from_secs(5 * 60);

/// Maximum characters per Telegram message.
///
/// Why: Telegram's hard cap is 4096 chars per message. Long ctrl responses are
/// split on the last newline before this boundary so we never cut mid-line.
const MAX_TELEGRAM_MESSAGE: usize = 4096;

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
struct ChatSession {
    project_path: PathBuf,
    history: Vec<ConversationTurn>,
    /// Active persona for this chat (#457).
    ///
    /// Why: `/switch <persona>` must persist across turns so subsequent
    /// messages route through `run_pm_task_with_persona` instead of the
    /// default ctrl/PM agent. `None` means "use the default ctrl runner".
    active_persona: Option<String>,
}

impl ChatSession {
    fn new(project_path: PathBuf) -> Self {
        Self {
            project_path,
            history: Vec::new(),
            active_persona: None,
        }
    }
}

/// Check whether a persona TOML exists under the user's home config dir.
///
/// Why (#457): `/switch <name>` must validate that the persona actually
/// resolves before storing it on the session — otherwise the next turn
/// would fail in `run_pm_task_with_persona` with a load error. We check
/// `~/.open-mpm/agents/<name>.toml` as a fallback after the project-local
/// path so user-level persona definitions also work.
/// What: Returns `true` iff `$HOME/.open-mpm/agents/<name>.toml` is a file.
/// Test: Indirectly via the `/switch` handler.
fn home_persona_exists(name: &str) -> bool {
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

/// Map of `ChatId` -> per-chat session, shared across handlers.
type SessionMap = Arc<Mutex<HashMap<ChatId, ChatSession>>>;

/// Map of `ChatId` -> instant the chat was paired, shared across handlers.
///
/// Why: #334 introduces a pairing gate. Only chats with an entry in this map
/// may dispatch to ctrl; everyone else gets a "🔒 Not paired" reply. Reads
/// dominate writes (every message reads, only `/pair` writes), so we use
/// `RwLock`.
type PairedChats = Arc<RwLock<HashMap<ChatId, Instant>>>;

/// On-disk record of a single paired chat.
///
/// Why (#467): `PairedChats` lives in memory only; every restart loses every
/// pairing, forcing users to re-run `/start` + `/pair` on every harness
/// upgrade. Persisting a minimal record under `~/.open-mpm/state/` survives
/// restarts without leaking any user content.
/// What: chat-id (Telegram's `i64`) plus the wall-clock timestamp of pairing.
/// `Instant` is monotonic and unsuitable for persistence, so we store
/// `DateTime<Utc>` and reconstruct `Instant::now()` on load — the absolute
/// pairing time is only used for diagnostic logs, not for expiry decisions.
/// Test: `paired_state_round_trip()` exercises save+load with a tempdir.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairedChatRecord {
    chat_id: i64,
    paired_at: DateTime<Utc>,
}

/// On-disk container for the paired-chats file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PairedChatsFile {
    paired_chats: Vec<PairedChatRecord>,
}

/// Resolve the absolute path of the paired-chats state file.
///
/// Why: We want the *user-level* `~/.open-mpm/state/` directory (shared across
/// projects), NOT the project-local `.open-mpm/state/`. Falls back to a
/// relative path when `HOME` is unset so we never panic on weird sandboxes.
fn paired_chats_state_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".open-mpm")
        .join("state")
        .join("telegram-paired.json")
}

/// Load persisted paired chats from disk.
///
/// Why (#467): On startup, restore the pairing map so users don't have to
/// re-pair every time the harness restarts.
/// What: Reads `state_path` as JSON. Missing file -> empty map (first run).
/// Parse errors -> log a warning and return empty map (never panic). The
/// stored `DateTime<Utc>` is discarded; we use `Instant::now()` as a stand-in
/// since the value is only consumed by diagnostic logging.
/// Test: `paired_state_round_trip` covers happy-path; missing-file and
/// malformed-JSON branches are intentionally fail-open (no panic).
async fn load_paired_chats(state_path: &Path) -> PairedChats {
    let bytes = match tokio::fs::read(state_path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Arc::new(RwLock::new(HashMap::new()));
        }
        Err(e) => {
            warn!(path = %state_path.display(), error = %e, "failed to read paired-chats state; starting empty");
            return Arc::new(RwLock::new(HashMap::new()));
        }
    };
    let parsed: PairedChatsFile = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(path = %state_path.display(), error = %e, "failed to parse paired-chats state; starting empty");
            return Arc::new(RwLock::new(HashMap::new()));
        }
    };
    let now = Instant::now();
    let mut map: HashMap<ChatId, Instant> = HashMap::with_capacity(parsed.paired_chats.len());
    for rec in parsed.paired_chats {
        // `Instant` cannot represent past wall-clock times; use `now` as a
        // stand-in. The exact value is only used by diagnostic logging.
        map.insert(ChatId(rec.chat_id), now);
    }
    info!(count = map.len(), path = %state_path.display(), "loaded paired chats");
    Arc::new(RwLock::new(map))
}

/// Persist the paired-chats map to disk atomically.
///
/// Why (#467): Survives harness restarts so a successful `/pair` is durable.
/// What: Snapshots the in-memory map under a read lock, serializes to JSON,
/// then writes via `<path>.tmp` + `rename` for atomic replacement. Creates
/// the parent directory on demand. Never panics: any IO error is logged and
/// returned; callers in the `/pair` handler intentionally swallow the error
/// because losing persistence on disk-full or perms is recoverable on next
/// successful save.
async fn save_paired_chats(paired: &PairedChats, state_path: &Path) -> Result<()> {
    if let Some(parent) = state_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            anyhow!(
                "failed to create paired-chats state dir {}: {e}",
                parent.display()
            )
        })?;
    }
    let snapshot: Vec<PairedChatRecord> = {
        let guard = paired.read().await;
        guard
            .keys()
            .map(|cid| PairedChatRecord {
                chat_id: cid.0,
                paired_at: Utc::now(),
            })
            .collect()
    };
    let file = PairedChatsFile {
        paired_chats: snapshot,
    };
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|e| anyhow!("failed to serialize paired-chats: {e}"))?;
    let tmp_path = state_path.with_extension("json.tmp");
    tokio::fs::write(&tmp_path, &json).await.map_err(|e| {
        anyhow!(
            "failed to write paired-chats tmp file {}: {e}",
            tmp_path.display()
        )
    })?;
    tokio::fs::rename(&tmp_path, state_path)
        .await
        .map_err(|e| {
            anyhow!(
                "failed to rename paired-chats tmp -> {}: {e}",
                state_path.display()
            )
        })?;
    Ok(())
}

/// Resolve the absolute path of the Telegram daemon PID file.
///
/// Why (#single-instance): Multiple `--telegram` processes polling
/// `getUpdates` concurrently trigger Telegram's `TerminatedByOtherGetUpdates`
/// error and fight over updates. A PID file in the user-level
/// `~/.open-mpm/state/` directory lets a starting daemon detect an
/// already-running peer. Falls back to a relative path when `HOME` is unset
/// so we never panic on weird sandboxes.
/// What: Returns `$HOME/.open-mpm/state/telegram.pid`.
fn telegram_pid_file_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".open-mpm").join("state").join("telegram.pid")
}

/// Check whether process `pid` is alive by sending signal 0 (no-op signal).
///
/// Why: `kill(pid, 0)` performs the permission/existence checks without
/// delivering a signal — the standard POSIX liveness probe. Used to decide
/// whether an existing PID file represents a live daemon or a stale lock.
/// What: Returns `true` iff `libc::kill(pid, 0)` succeeds.
/// Test: A live process (our own PID) returns true; an absurd PID returns
/// false — covered by `telegram_pid_alive_*` tests.
fn telegram_pid_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 only probes; it never mutates process
    // state. `pid` is a plain integer and any value is a valid argument —
    // the kernel rejects invalid ones via errno, which `== 0` filters out.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// RAII guard that owns the Telegram daemon PID file.
///
/// Why: The PID file must be removed on *every* exit path — normal return,
/// `?` early-return, panic, and SIGINT (the dispatcher's `enable_ctrlc_handler`
/// terminates the long-poll loop, after which `run_telegram_bot` returns and
/// this guard drops). A `Drop` impl is the only construct that fires on all
/// of those without scattering cleanup calls.
/// What: `acquire()` enforces single-instance semantics and writes the PID
/// file; `Drop` removes it. Holds the path so `Drop` needs no recomputation.
/// Test: `telegram_pid_guard_*` tests exercise acquire/stale/drop behavior.
struct TelegramPidGuard {
    path: PathBuf,
}

impl TelegramPidGuard {
    /// Acquire the single-instance lock for the Telegram daemon.
    ///
    /// Why: Prevents two `--telegram` processes from racing on `getUpdates`.
    /// What: If a PID file exists and the PID inside is still alive, returns
    /// an error (caller should exit). A stale PID file (process dead) is
    /// overwritten. On success, writes the current PID and returns a guard
    /// whose `Drop` cleans up the file.
    /// Test: `telegram_pid_guard_acquire_writes_file`,
    /// `telegram_pid_guard_stale_is_overwritten`.
    fn acquire(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("failed to create state dir {}: {e}", parent.display()))?;
        }

        // If a PID file exists, decide live vs. stale.
        if let Ok(contents) = std::fs::read_to_string(&path)
            && let Ok(existing_pid) = contents.trim().parse::<i32>()
            && telegram_pid_alive(existing_pid)
        {
            return Err(anyhow!(
                "Telegram daemon already running (PID {existing_pid}). \
                         Stop it before starting another, or delete {} if you \
                         are sure it is stale.",
                path.display()
            ));
        }
        // Stale lock: the recorded process is gone. Fall through to
        // overwrite it.
        // Unparseable contents are treated as stale too.

        let pid = std::process::id();
        std::fs::write(&path, pid.to_string())
            .map_err(|e| anyhow!("failed to write PID file {}: {e}", path.display()))?;

        Ok(Self { path })
    }
}

impl Drop for TelegramPidGuard {
    /// Best-effort removal of the PID file on daemon exit.
    ///
    /// Why: Leaving a stale file behind would force the next start to treat
    /// it as a stale lock — harmless but noisy. Errors are intentionally
    /// swallowed: there is nothing useful to do during teardown.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Map of pending pairing codes keyed by raw chat-id (`i64`).
///
/// Why (#334): The pairing code is generated **in the REPL** (trusted
/// terminal), not on Telegram. The REPL writes the code under the sentinel
/// key `SENTINEL_PAIRING_CHAT_ID` (= `i64::MAX`). When a `/pair <code>`
/// arrives from Telegram, we look up the sentinel entry; on a match the
/// real `ChatId` is promoted to `paired`. This means an attacker who
/// controls the bot cannot self-authorize — they must also have shell
/// access to the host running the REPL.
/// What: `Arc<tokio::sync::Mutex<HashMap<i64, (String, Instant)>>>`. The
/// raw `i64` (not `ChatId`) keeps the REPL free of teloxide types.
pub type PendingPairs = Arc<Mutex<HashMap<i64, (String, Instant)>>>;

/// Sentinel chat-id under which the REPL stores the next pending code.
///
/// Why: A real Telegram chat-id never equals `i64::MAX` in practice, so this
/// is a safe out-of-band key for "the next /pair attempt from any chat".
pub const SENTINEL_PAIRING_CHAT_ID: i64 = i64::MAX;

/// Construct a fresh, empty `PendingPairs` shared across REPL + bot task.
pub fn new_pending_pairs() -> PendingPairs {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Generate and store a REPL-issued pairing code under the sentinel key.
///
/// Why (#334): Called from `/telegram pair` in the REPL. The next `/pair
/// <code>` arriving on Telegram (from any chat) can claim it. Overwrites
/// any prior pending sentinel entry — only the most recent REPL-issued
/// code is honoured.
/// What: Returns the 6-digit code so the REPL can display it.
/// Test: `repl_issued_code_lands_under_sentinel` exercises the flow.
pub async fn issue_repl_pairing_code(pending: &PendingPairs) -> String {
    let code = generate_pairing_code();
    let mut map = pending.lock().await;
    map.insert(SENTINEL_PAIRING_CHAT_ID, (code.clone(), Instant::now()));
    code
}

/// Generate a random 6-digit pairing code (zero-padded).
///
/// Why: 6 digits gives ~1M codes — plenty for a human-friendly handoff over a
/// log line, while still being short enough to type on a phone.
/// What: Uses `rand::random::<u32>() % 1_000_000` and zero-pads with `{:06}`.
/// Test: `pairing_code_is_six_digits` asserts the format.
fn generate_pairing_code() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

/// Slash commands exposed to Telegram users.
///
/// Why: teloxide's `BotCommands` derive auto-generates parsing + a help string
/// from the `description` attributes. Routing is done in the dptree handler.
/// What: `/start`, `/help`, `/connect <path>`, `/clear`, `/status`.
/// Test: Compile-time check via `BotCommands::descriptions()`.
#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "open-mpm Telegram bot commands:"
)]
enum Command {
    #[command(description = "Welcome message and pairing code")]
    Start,
    #[command(description = "Authorize this chat with a pairing code: /pair <code>")]
    Pair(String),
    #[command(description = "List available commands")]
    Help,
    #[command(description = "Set the project path for this chat: /connect <path>")]
    Connect(String),
    #[command(description = "Reset conversation history for this chat")]
    Clear,
    #[command(description = "Show current project path and LLM backend")]
    Status,
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

/// Outcome of a `/pair <code>` attempt. Pure for unit testing.
///
/// Why: We want to unit-test the state-machine without the teloxide types in
/// the loop. `verify_pair_attempt` returns one of these and the handler turns
/// it into Telegram replies + map mutations.
/// Test: `pair_*` tests below.
#[derive(Debug, PartialEq, Eq)]
enum PairOutcome {
    /// No `/start` was issued for this chat (no pending code).
    NoPending,
    /// The pending code is past its TTL.
    Expired,
    /// The provided code does not match the pending code.
    Mismatch,
    /// The provided code matches and is within TTL — caller must promote the
    /// chat to paired.
    Success,
}

/// Verify a `/pair` attempt against a pending entry.
///
/// Why: Pure function so we can exhaustively test the state machine without
/// spinning up a teloxide bot. The caller is responsible for the side effects
/// (removing the pending entry, inserting into paired, sending the reply).
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

/// Dispatch a parsed slash command.
// Why: teloxide's dptree dispatch passes injected state as positional
// arguments; this function reflects the dispatch signature exactly so a
// wrapper struct would just have to be unpacked at every call site.
#[allow(clippy::too_many_arguments)]
async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    sessions: SessionMap,
    project_path: Arc<PathBuf>,
    paired: PairedChats,
    paired_state_path: Arc<PathBuf>,
    pending: PendingPairs,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id;

    // #334: Gate every command except /start and /pair behind the pairing
    // check. Unpaired chats get a uniform "send /start" prompt instead of
    // partial information about the bot.
    let is_unauthenticated_cmd = matches!(cmd, Command::Start | Command::Pair(_));
    if !is_unauthenticated_cmd {
        let is_paired = paired.read().await.contains_key(&chat_id);
        if !is_paired {
            bot.send_message(chat_id, "🔒 Not paired. Send /start to begin.")
                .parse_mode(ParseMode::Html)
                .await?;
            return Ok(());
        }
    }

    match cmd {
        Command::Start => {
            // #334: The pairing code is generated by the REPL (trusted
            // terminal), NOT here. We only tell the user how to obtain it.
            // This prevents Telegram-side attackers from self-authorizing
            // even if they own the bot — they'd also need shell access to
            // the host running the REPL.
            tracing::info!(
                chat_id = chat_id.0,
                "Telegram /start received; instructing user to pair via REPL"
            );
            let _ = &pending; // pending is populated by the REPL, not here.
            let text = concat!(
                "🔐 <b>Pairing required</b>\n\n",
                "To link this Telegram chat, go to your open-mpm REPL and run:\n\n",
                "  <code>/telegram pair</code>\n\n",
                "Then send the code here:  <code>/pair &lt;code&gt;</code>\n\n",
                "(Codes expire in 5 minutes.)"
            );
            bot.send_message(chat_id, text)
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Command::Pair(code_arg) => {
            let provided = code_arg.trim().to_string();
            if provided.is_empty() {
                bot.send_message(chat_id, "Usage: <code>/pair &lt;code&gt;</code>")
                    .parse_mode(ParseMode::Html)
                    .await?;
                return Ok(());
            }
            let now = Instant::now();
            // #334: Look up first by per-chat key (legacy / future), then
            // fall back to the REPL-issued sentinel entry. The sentinel is
            // the standard path now.
            let (outcome, matched_key) = {
                let map = pending.lock().await;
                let chat_outcome =
                    verify_pair_attempt(map.get(&chat_id.0), &provided, now, PAIRING_CODE_TTL);
                if matches!(
                    chat_outcome,
                    PairOutcome::Success | PairOutcome::Mismatch | PairOutcome::Expired
                ) {
                    (chat_outcome, chat_id.0)
                } else {
                    let sentinel_outcome = verify_pair_attempt(
                        map.get(&SENTINEL_PAIRING_CHAT_ID),
                        &provided,
                        now,
                        PAIRING_CODE_TTL,
                    );
                    (sentinel_outcome, SENTINEL_PAIRING_CHAT_ID)
                }
            };
            match outcome {
                PairOutcome::NoPending => {
                    bot.send_message(chat_id, "No pending pairing. Send /start first.")
                        .await?;
                }
                PairOutcome::Expired => {
                    pending.lock().await.remove(&matched_key);
                    bot.send_message(chat_id, "Code expired. Send /start to get a new code.")
                        .await?;
                }
                PairOutcome::Mismatch => {
                    bot.send_message(chat_id, "Invalid code.").await?;
                }
                PairOutcome::Success => {
                    pending.lock().await.remove(&matched_key);
                    paired.write().await.insert(chat_id, now);
                    // #467: Persist the new pairing so it survives restart.
                    // We deliberately log-and-continue on IO errors: losing
                    // persistence is recoverable on the next successful save,
                    // and we don't want a disk-full to block the user reply.
                    if let Err(e) = save_paired_chats(&paired, &paired_state_path).await {
                        warn!(
                            chat_id = chat_id.0,
                            error = %e,
                            "failed to persist paired-chats state"
                        );
                    }
                    info!(
                        chat_id = chat_id.0,
                        matched_via = matched_key,
                        "Chat paired successfully"
                    );
                    bot.send_message(
                        chat_id,
                        "✅ <b>Paired successfully.</b> You can now send messages.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await?;
                }
            }
        }
        Command::Help => {
            let text = format!(
                "<b>Commands</b>\n\n<pre>{}</pre>",
                html::escape(&Command::descriptions().to_string())
            );
            bot.send_message(chat_id, text)
                .parse_mode(ParseMode::Html)
                .await?;
        }
        Command::Connect(path_arg) => {
            let trimmed = path_arg.trim();
            if trimmed.is_empty() {
                bot.send_message(chat_id, "Usage: <code>/connect &lt;path&gt;</code>")
                    .parse_mode(ParseMode::Html)
                    .await?;
                return Ok(());
            }
            let new_path = PathBuf::from(trimmed);
            if !new_path.is_dir() {
                bot.send_message(
                    chat_id,
                    format!(
                        "Path does not exist or is not a directory: <code>{}</code>",
                        html::escape(trimmed)
                    ),
                )
                .parse_mode(ParseMode::Html)
                .await?;
                return Ok(());
            }
            {
                let mut map = sessions.lock().await;
                let entry = map
                    .entry(chat_id)
                    .or_insert_with(|| ChatSession::new((*project_path).clone()));
                entry.project_path = new_path.clone();
                // /connect intentionally does NOT clear history — the user
                // may want to reference earlier turns when changing project.
            }
            bot.send_message(
                chat_id,
                format!(
                    "Connected to <code>{}</code>",
                    html::escape(&new_path.display().to_string())
                ),
            )
            .parse_mode(ParseMode::Html)
            .await?;
        }
        Command::Clear => {
            let mut map = sessions.lock().await;
            if let Some(session) = map.get_mut(&chat_id) {
                session.history.clear();
            }
            bot.send_message(chat_id, "Conversation history cleared.")
                .await?;
        }
        Command::Status => {
            let map = sessions.lock().await;
            let path = map
                .get(&chat_id)
                .map(|s| s.project_path.clone())
                .unwrap_or_else(|| (*project_path).clone());
            let history_len = map.get(&chat_id).map(|s| s.history.len()).unwrap_or(0);
            drop(map);

            // #295: pass `None` here — the telegram status command has no
            // single agent context. Passing None means claude-code will only
            // surface if some downstream actually opts in (which they won't
            // through this status path), and falls through to OpenRouter /
            // Anthropic-direct when those env vars are set.
            let llm_label = crate::llm::credentials::pick_credentials(None)
                .map(|c| c.label())
                .unwrap_or("none");

            let text = format!(
                "<b>Status</b>\n\n\
                Project: <code>{}</code>\n\
                Turns:   {}\n\
                LLM:     <code>{}</code>",
                html::escape(&path.display().to_string()),
                history_len,
                html::escape(llm_label)
            );
            bot.send_message(chat_id, text)
                .parse_mode(ParseMode::Html)
                .await?;
        }
    }
    Ok(())
}

/// Forward a plain-text message to ctrl and reply with the result.
async fn handle_message(
    bot: Bot,
    msg: Message,
    sessions: SessionMap,
    project_path: Arc<PathBuf>,
    paired: PairedChats,
) -> ResponseResult<()> {
    let chat_id = msg.chat.id;
    let user_msg_id = msg.id;
    let text = match msg.text() {
        Some(t) => t.to_string(),
        None => return Ok(()),
    };

    // #334: Gate the LLM dispatch behind the pairing check. We do this before
    // showing the typing indicator so the user gets immediate feedback.
    if !paired.read().await.contains_key(&chat_id) {
        bot.send_message(chat_id, "🔒 Not paired. Send /start to begin.")
            .parse_mode(ParseMode::Html)
            .await?;
        return Ok(());
    }

    // #457: Intercept `/switch <persona>` BEFORE ctrl dispatch. ctrl's
    // text path passes the message verbatim to the LLM — it has no slash
    // command awareness. We must handle persona switching ourselves and
    // store the choice on the session so subsequent turns route through
    // `run_pm_task_with_persona`.
    if text.starts_with("/switch") {
        let arg = text.trim_start_matches("/switch").trim().to_string();
        if arg.is_empty() {
            // No arg: report current persona instead of switching.
            let map = sessions.lock().await;
            let current = map
                .get(&chat_id)
                .and_then(|s| s.active_persona.clone())
                .unwrap_or_else(|| "ctrl (default)".to_string());
            drop(map);
            bot.send_message(
                chat_id,
                format!(
                    "Active persona: {current}\n\nUse /switch <name> to change. \
                     Available: ctrl, izzie, cto (or cto-assistant)"
                ),
            )
            .await?;
            return Ok(());
        }
        // Canonicalize aliases to match REPL's /switch behavior
        // (src/repl/agent_commands.rs): `cto`/`cto assistant` -> `cto-assistant`,
        // and `default` -> `ctrl`. Pass through any other agent TOML stems
        // unchanged so user-authored personas keep working.
        let stem = match arg.to_lowercase().trim() {
            "ctrl" | "default" => "ctrl".to_string(),
            "izzie" => "izzie".to_string(),
            "cto" | "cto-assistant" | "cto assistant" => "cto-assistant".to_string(),
            _ => arg.clone(),
        };
        // `/switch ctrl` / `/switch default` clears the persona, restoring
        // the default ctrl/PM runner used before any switch.
        if stem == "ctrl" {
            let mut map = sessions.lock().await;
            let entry = map
                .entry(chat_id)
                .or_insert_with(|| ChatSession::new((*project_path).clone()));
            entry.active_persona = None;
            drop(map);
            bot.send_message(chat_id, "✓ Switched back to ctrl (default)")
                .await?;
            return Ok(());
        }
        // Validate the persona resolves. Mirror ctrl's resolution order:
        // project-local agents dir first, then ~/.open-mpm/agents/.
        let session_project_path = {
            let map = sessions.lock().await;
            map.get(&chat_id)
                .map(|s| s.project_path.clone())
                .unwrap_or_else(|| (*project_path).clone())
        };
        let persona_path = session_project_path
            .join(".open-mpm")
            .join("agents")
            .join(format!("{stem}.toml"));
        if persona_path.exists() || home_persona_exists(&stem) {
            let mut map = sessions.lock().await;
            let entry = map
                .entry(chat_id)
                .or_insert_with(|| ChatSession::new((*project_path).clone()));
            entry.active_persona = Some(stem.clone());
            drop(map);
            bot.send_message(chat_id, format!("✓ Switched to {arg}"))
                .await?;
        } else {
            bot.send_message(
                chat_id,
                format!("Unknown persona: {arg}. Available: ctrl, izzie, cto (or cto-assistant)"),
            )
            .await?;
        }
        return Ok(());
    }

    // Show "typing…" indicator while we wait on the LLM. Best-effort: if it
    // fails we still try the dispatch.
    let _ = bot.send_chat_action(chat_id, ChatAction::Typing).await;

    // Snapshot the project + history + active persona under the lock so we
    // don't hold it across the multi-second LLM call.
    let (path, history_snapshot, active_persona) = {
        let mut map = sessions.lock().await;
        let entry = map
            .entry(chat_id)
            .or_insert_with(|| ChatSession::new((*project_path).clone()));
        (
            entry.project_path.clone(),
            entry.history.clone(),
            entry.active_persona.clone(),
        )
    };

    // #457: Route to the persona-specific runner if `/switch <persona>` is
    // active on this chat. Otherwise the default ctrl agent handles it.
    //
    // #468: The default (no /switch) path used to call
    // `run_pm_task_with_history`, which internally resolves the agent via
    // `resolve_agent_config` — and that resolver prefers
    // `{project}/.open-mpm/agents/pm.toml` over ctrl.toml. When the harness
    // runs inside the open-mpm repo (or any project shipping a pm.toml),
    // Telegram chats silently loaded the heavy sonnet PM prompt + full
    // delegation toolset instead of the lightweight ctrl persona that
    // ctrl.toml configures (e.g. `model = "ollama/qwen3:30b"`). The REPL
    // already routes its conversational ctrl chat through
    // `run_pm_task_with_persona("ctrl", …)` (see src/repl/dispatch.rs and
    // `resolve_ctrl_agent_config`), which loads ctrl.toml directly. We
    // mirror that pattern here so Telegram and REPL agree on which agent
    // config wins by default.
    let persona_name = active_persona.as_deref().unwrap_or("ctrl");
    let result = ctrl::run_pm_task_with_persona(
        &path,
        persona_name,
        &text,
        &history_snapshot,
        None,
        ctrl::SessionOverrides::default(),
    )
    .await;

    let response_text = match result {
        Ok(reply) => {
            // Persist the new turn. Do this before formatting so a parse
            // error in the response doesn't drop the LLM round-trip.
            let mut map = sessions.lock().await;
            let entry = map
                .entry(chat_id)
                .or_insert_with(|| ChatSession::new((*project_path).clone()));
            entry.history.push(ConversationTurn {
                user: text.clone(),
                assistant: reply.clone(),
            });
            drop(map);
            markdown_to_html_safe(&reply)
        }
        Err(e) => {
            warn!(chat_id = %chat_id.0, error = %e, "ctrl dispatch failed");
            // #466: Surface the actual error to the user instead of a fixed
            // "LLM backend not configured" string. The previous message was
            // misleading: any failure (network, parse error, upstream 5xx)
            // was reported as a credential problem. Including the real error
            // makes debugging dramatically easier for operators.
            format!("⚠️ Request failed: {e}")
        }
    };

    send_long_html(&bot, chat_id, user_msg_id, &response_text).await;
    Ok(())
}

/// Send a (possibly long) HTML-formatted reply, splitting on the 4096-char
/// boundary at newlines where possible.
///
/// Why: Telegram rejects messages > 4096 chars with `MESSAGE_TOO_LONG`. We
/// split on newlines so we don't cut mid-tag (which would corrupt HTML
/// rendering). Reply parameters are attached only to the last chunk so the
/// thread is anchored to the user's message without spamming reply arrows on
/// every chunk.
async fn send_long_html(bot: &Bot, chat_id: ChatId, user_msg_id: MessageId, text: &str) {
    let chunks = split_message(text, MAX_TELEGRAM_MESSAGE);
    let total = chunks.len();
    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i == total - 1;
        let mut req = bot.send_message(chat_id, chunk).parse_mode(ParseMode::Html);
        if is_last {
            req = req.reply_parameters(ReplyParameters::new(user_msg_id));
        }
        if let Err(e) = req.await {
            // Fallback: if HTML parsing fails for any reason (e.g. unbalanced
            // tags from naive markdown conversion), retry as plain text so
            // the user still gets the content.
            warn!(chat_id = %chat_id.0, error = %e, "HTML send failed; retrying as plain text");
            let plain = strip_html_tags(chunk);
            let mut retry = bot.send_message(chat_id, plain);
            if is_last {
                retry = retry.reply_parameters(ReplyParameters::new(user_msg_id));
            }
            if let Err(e2) = retry.await {
                warn!(chat_id = %chat_id.0, error = %e2, "plain-text fallback also failed");
            }
        }
    }
}

/// Split `text` into chunks of at most `max_len` chars, preferring to break
/// on newlines.
///
/// Why: Telegram's hard cap is 4096 chars. Hard-splitting mid-line yields
/// ugly output and can break HTML tags. We prefer the rightmost newline in
/// the first `max_len` chars, falling back to a hard split when there is no
/// newline (e.g. a 5000-char single line).
/// What: Returns a `Vec<String>` whose concatenation equals `text`.
/// Test: `split_message_short`, `split_message_newline_boundary`,
/// `split_message_hard_split` below.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > max_len {
        // Find the last char boundary at-or-before max_len so we never slice
        // mid-UTF-8 sequence.
        let mut boundary = max_len;
        while boundary > 0 && !remaining.is_char_boundary(boundary) {
            boundary -= 1;
        }
        if boundary == 0 {
            // Pathological: a single char wider than max_len. Push the whole
            // remaining and bail to avoid an infinite loop.
            chunks.push(remaining.to_owned());
            return chunks;
        }
        // Prefer to break on the rightmost newline within [0, boundary).
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

/// Convert ctrl's Markdown-ish output into Telegram-safe HTML.
///
/// Why: ctrl replies are Markdown (with code fences, inline code, bold). The
/// safest rendering on Telegram is HTML — but we have to escape user-supplied
/// content first, then re-introduce a small whitelist of formatting so we
/// never emit a tag Telegram doesn't support (which would 400 the message).
/// What: Strips ANSI escapes, escapes <, >, &, then converts ```lang ... ```
/// into `<pre><code>...</code></pre>`, `code` into `<code>code</code>`, and
/// `**bold**` into `<b>bold</b>`. Anything else passes through as plain text.
/// Test: `markdown_to_html_safe_*` below.
fn markdown_to_html_safe(input: &str) -> String {
    // Strip ANSI escapes first — ctrl's output may contain colour codes from
    // tool runners that have no place in Telegram chat.
    let cleaned = strip_ansi(input);
    let escaped = html::escape(&cleaned);

    // Convert fenced code blocks. We use a simple state machine over lines so
    // we don't accidentally rewrite triple-backticks inside user content
    // (the `escaped` step has already turned any user-visible `<` into `&lt;`).
    let mut out = String::with_capacity(escaped.len() + 32);
    let mut in_fence = false;
    for line in escaped.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        if trimmed.starts_with("```") {
            if in_fence {
                out.push_str("</code></pre>");
                if line.ends_with('\n') {
                    out.push('\n');
                }
                in_fence = false;
            } else {
                out.push_str("<pre><code>");
                if line.ends_with('\n') {
                    // Drop the language hint line entirely — Telegram <code>
                    // doesn't render syntax classes anyway.
                }
                in_fence = true;
            }
            continue;
        }
        out.push_str(line);
    }
    if in_fence {
        out.push_str("</code></pre>");
    }

    // Inline formatting: inline code FIRST, then bold. Why the reversed
    // order vs. the obvious "bold first"? If we ran bold first, a string
    // like "`foo **bar** baz`" would have its `**` converted to `<b>` *inside*
    // the code span, corrupting it (Telegram <code> renders tags literally,
    // so the user would see "<b>bar</b>" in monospace). Doing code first
    // means `**` markers inside backticks get sealed inside a <code> tag
    // before the bold pass ever sees them, so the bold pass can only
    // affect text outside code spans.
    let coded = convert_pairs(&out, "`", "<code>", "</code>");
    convert_pairs_outside_tag(&coded, "**", "<b>", "</b>", "<code>", "</code>")
}

/// Like `convert_pairs`, but skips ranges that are already inside the named
/// HTML tag (used to prevent bold/italic conversion inside `<code>` spans
/// that we just emitted, which would re-corrupt the code).
///
/// Why: Even with code-first ordering, a code span like "`a` **b** `c`"
/// has plain text outside the spans where bold conversion is desired. But
/// "`**x**`" would already be wrapped — we must not touch tokens inside
/// `<code>…</code>`. Naive `find(delim)` doesn't know about tags. This
/// helper walks the string and toggles a "skip" flag whenever it enters /
/// exits the named tag pair, only running the conversion on outside text.
/// Test: `convert_pairs_outside_tag_skips_code`.
fn convert_pairs_outside_tag(
    input: &str,
    delim: &str,
    open: &str,
    close: &str,
    skip_open: &str,
    skip_close: &str,
) -> String {
    let mut out = String::with_capacity(input.len());
    let mut buf = String::new();
    let mut rest = input;
    loop {
        let next_skip = rest.find(skip_open);
        match next_skip {
            None => {
                buf.push_str(rest);
                break;
            }
            Some(idx) => {
                buf.push_str(&rest[..idx]);
                // Flush converted buffer.
                out.push_str(&convert_pairs(&buf, delim, open, close));
                buf.clear();
                // Find the matching close. If none, append the rest as-is
                // (defensive — shouldn't happen since we just emitted these).
                let after_open = &rest[idx + skip_open.len()..];
                match after_open.find(skip_close) {
                    None => {
                        out.push_str(&rest[idx..]);
                        return out;
                    }
                    Some(close_idx) => {
                        let span_end = idx + skip_open.len() + close_idx + skip_close.len();
                        out.push_str(&rest[idx..span_end]);
                        rest = &rest[span_end..];
                    }
                }
            }
        }
    }
    out.push_str(&convert_pairs(&buf, delim, open, close));
    out
}

/// Replace paired `delim` markers with `open`/`close` HTML tags.
///
/// Why: Markdown bold (`**x**`) and inline code (`` `x` ``) are both
/// delimiter-paired. We treat them identically: count occurrences, alternate
/// open/close, leave any unpaired trailing delim as a literal so we don't
/// emit unbalanced HTML (which Telegram would reject).
/// Test: `convert_pairs_alternates_open_close` below.
fn convert_pairs(input: &str, delim: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut next_is_open = true;
    while let Some(idx) = rest.find(delim) {
        out.push_str(&rest[..idx]);
        // Lookahead: is there a closing delim later? If not, treat this as
        // literal text.
        let after = &rest[idx + delim.len()..];
        if next_is_open && !after.contains(delim) {
            out.push_str(delim);
            rest = after;
            continue;
        }
        if next_is_open {
            out.push_str(open);
        } else {
            out.push_str(close);
        }
        next_is_open = !next_is_open;
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Strip ANSI escape sequences (CSI / SGR) so terminal colour codes don't
/// leak into Telegram messages.
fn strip_ansi(s: &str) -> String {
    // Reuse the project's existing strip-ansi crate via the `strip_ansi_escapes` dep.
    // `strip_str` returns String in newer versions; the result is a clean
    // String regardless of crate version, so just pass it through.
    strip_ansi_escapes::strip_str(s)
}

/// Last-resort: remove all `<...>` tags and unescape HTML entities so the
/// plain-text fallback shows readable content instead of raw HTML.
///
/// Why: The send-as-HTML path escapes `<`, `>`, `&` to `&lt;`, `&gt;`, `&amp;`
/// before sending. If Telegram rejects the HTML (e.g. unbalanced tags from a
/// bizarre LLM reply), the fallback path used to leak those entities verbatim
/// to the user ("a &lt; b"). We strip tags AND unescape entities here so the
/// fallback message is human-readable.
/// What: First removes `<…>` tags, then replaces `&lt;`, `&gt;`, `&quot;`,
/// `&#39;`, `&amp;` (in this order — `&amp;` must run last to avoid
/// double-decoding).
/// Test: `strip_html_tags_unescapes_entities`.
fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Unescape entities. Order matters: `&amp;` must come last so we don't
    // turn `&amp;lt;` into `<` (it should stay as `&lt;`).
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", MAX_TELEGRAM_MESSAGE);
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
    fn markdown_to_html_safe_escapes_lt_gt() {
        let out = markdown_to_html_safe("a < b > c");
        assert!(out.contains("&lt;"));
        assert!(out.contains("&gt;"));
    }

    #[test]
    fn markdown_to_html_safe_fence_to_pre() {
        let input = "before\n```rust\nlet x = 1;\n```\nafter";
        let out = markdown_to_html_safe(input);
        assert!(out.contains("<pre><code>"), "got: {}", out);
        assert!(out.contains("</code></pre>"), "got: {}", out);
    }

    #[test]
    fn markdown_to_html_safe_inline_code() {
        let out = markdown_to_html_safe("call `foo()` then");
        assert!(out.contains("<code>foo()</code>"), "got: {}", out);
    }

    #[test]
    fn markdown_to_html_safe_bold() {
        let out = markdown_to_html_safe("this is **important**!");
        assert!(out.contains("<b>important</b>"), "got: {}", out);
    }

    #[test]
    fn convert_pairs_alternates_open_close() {
        let out = convert_pairs("a `b` c `d` e", "`", "<c>", "</c>");
        assert_eq!(out, "a <c>b</c> c <c>d</c> e");
    }

    #[test]
    fn convert_pairs_unbalanced_passes_through() {
        let out = convert_pairs("a `b c", "`", "<c>", "</c>");
        assert_eq!(out, "a `b c");
    }

    #[test]
    fn strip_html_tags_removes_tags() {
        assert_eq!(strip_html_tags("<b>hi</b> there"), "hi there");
    }

    /// #419: Plain-text fallback must unescape HTML entities.
    ///
    /// Why: When `markdown_to_html_safe` escapes `<` to `&lt;`, the HTML send
    /// path renders it correctly. But if the HTML send fails and we fall back
    /// to plain text via `strip_html_tags`, the user used to see literal
    /// `&lt;` characters. After the fix, entities are decoded so the user
    /// sees the original symbol.
    /// Test: Round-trip "a < b & c" through escape + strip.
    #[test]
    fn strip_html_tags_unescapes_entities() {
        let escaped = "a &lt; b &amp; c &gt; d &quot;e&quot; &#39;f&#39;";
        let plain = strip_html_tags(escaped);
        assert_eq!(plain, "a < b & c > d \"e\" 'f'");
    }

    /// #419: `&amp;` must decode last so encoded entities don't double-decode.
    ///
    /// Why: A string containing the literal text `&lt;` (user wrote "&lt;",
    /// not "<") would round-trip to `&amp;lt;`. The strip path must yield
    /// `&lt;`, not `<`.
    /// Test: Encode then strip; the literal entity must survive.
    #[test]
    fn strip_html_tags_does_not_double_decode() {
        // User content: literal "&lt;" → escaped to "&amp;lt;" by html::escape.
        let escaped = "raw &amp;lt; here";
        let plain = strip_html_tags(escaped);
        assert_eq!(plain, "raw &lt; here");
    }

    /// #419: Bold markers inside backticks must NOT become <b> tags.
    ///
    /// Why: A reply like `` `let x = **value**;` `` should render the `**`
    /// literally inside the code span. The pre-fix order ran bold first
    /// and produced "<code>let x = <b>value</b>;</code>", which Telegram
    /// renders as literal "<b>value</b>" in monospace.
    /// Test: Convert and assert no <b> tags appear inside the code span.
    #[test]
    fn markdown_to_html_safe_bold_inside_code_is_literal() {
        let out = markdown_to_html_safe("call `x = **literal**` here");
        assert!(out.contains("<code>x = **literal**</code>"), "got: {out}");
        assert!(!out.contains("<b>"), "<b> should not appear: {out}");
    }

    /// #419: Bold OUTSIDE code spans still works.
    ///
    /// Why: Reversing the conversion order must not regress the common case.
    /// Test: `**emph** and `code`` → bold on emph, code on code.
    #[test]
    fn markdown_to_html_safe_bold_outside_code_still_works() {
        let out = markdown_to_html_safe("**emph** and `code`");
        assert!(out.contains("<b>emph</b>"), "got: {out}");
        assert!(out.contains("<code>code</code>"), "got: {out}");
    }

    /// #419: convert_pairs_outside_tag skips inside <code> spans.
    ///
    /// Why: Direct unit test of the helper that powers the bold-after-code
    /// fix. Inside `<code>…</code>`, `**x**` must be left untouched.
    /// Test: Manually wrap a code span and verify bold conversion only
    /// touches the outside.
    #[test]
    fn convert_pairs_outside_tag_skips_code() {
        let input = "**a** <code>**b**</code> **c**";
        let out = convert_pairs_outside_tag(input, "**", "<B>", "</B>", "<code>", "</code>");
        assert_eq!(out, "<B>a</B> <code>**b**</code> <B>c</B>");
    }

    /// #419: convert_pairs_outside_tag handles unclosed code span defensively.
    ///
    /// Why: If `markdown_to_html_safe` ever emits an unclosed `<code>` (it
    /// shouldn't, but defense in depth matters), we must not loop or panic.
    /// Test: Input with `<code>` and no `</code>` returns the prefix
    /// converted plus the unclosed tail verbatim.
    #[test]
    fn convert_pairs_outside_tag_unclosed_does_not_panic() {
        let input = "**a** <code>tail";
        let out = convert_pairs_outside_tag(input, "**", "<B>", "</B>", "<code>", "</code>");
        assert_eq!(out, "<B>a</B> <code>tail");
    }

    /// #419: Empty input to split_message returns one empty chunk… or none?
    ///
    /// Why: The dispatch path can in principle hand `send_long_html` an empty
    /// string (e.g. an LLM that returns "" after error recovery). We must
    /// not panic, and we must not try to send a zero-length Telegram message
    /// (which would 400). Verify the split function returns a single
    /// empty-string chunk for empty input — the caller's iteration then
    /// hits `send_message(chat, "")` which Telegram itself rejects gracefully
    /// via the existing error fallback.
    /// Test: Empty in, single empty out.
    #[test]
    fn split_message_empty_input() {
        let chunks = split_message("", MAX_TELEGRAM_MESSAGE);
        assert_eq!(chunks, vec!["".to_string()]);
    }

    /// #419: split_message at exact boundary length stays one chunk.
    ///
    /// Why: Off-by-one in the `text.len() <= max_len` check would split
    /// strings that are exactly at the limit into two pieces, wasting a
    /// round-trip. Verify equality with max_len is one chunk.
    /// Test: 100-char string with max_len=100 → 1 chunk.
    #[test]
    fn split_message_exact_boundary() {
        let text = "a".repeat(100);
        let chunks = split_message(&text, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 100);
    }

    /// #419: Long fenced code block + bold around it survives conversion.
    ///
    /// Why: End-to-end check that fence + bold + escaping compose cleanly.
    /// This is the realistic shape of an LLM reply ("Here is the **fix**:
    /// ```rust\nfn x() {}\n```").
    /// Test: Verify the bold conversion happened, the fence is now <pre>,
    /// and the angle brackets inside the code are escaped.
    #[test]
    fn markdown_to_html_safe_realistic_reply() {
        let input = "Here is the **fix**:\n```rust\nfn x<T>() {}\n```\nDone.";
        let out = markdown_to_html_safe(input);
        assert!(out.contains("<b>fix</b>"), "got: {out}");
        assert!(out.contains("<pre><code>"), "got: {out}");
        assert!(
            out.contains("fn x&lt;T&gt;()"),
            "angle brackets must be escaped: {out}"
        );
        assert!(out.contains("</code></pre>"), "got: {out}");
    }

    #[test]
    fn pairing_code_is_six_digits() {
        // Why: Loop a few times to catch the zero-padding edge case where
        // rand happens to return a small number (e.g. 42 -> "000042").
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
        // Simulate "now" being TTL + 1s after issuance.
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
        // Within TTL.
        let now = issued + Duration::from_secs(60);
        let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
        assert_eq!(outcome, PairOutcome::Success);
    }

    /// #334: REPL-issued code lands under the sentinel key.
    ///
    /// Why: The new flow has the REPL (not Telegram) generate the code and
    /// store it under `SENTINEL_PAIRING_CHAT_ID`. Verifies that
    /// `issue_repl_pairing_code` populates the map at the sentinel key.
    /// Test: Call `issue_repl_pairing_code`, then assert the map has the
    /// returned code under `SENTINEL_PAIRING_CHAT_ID`.
    #[tokio::test]
    async fn repl_issued_code_lands_under_sentinel() {
        let pending = new_pending_pairs();
        let code = issue_repl_pairing_code(&pending).await;
        assert_eq!(code.len(), 6);
        let map = pending.lock().await;
        let entry = map.get(&SENTINEL_PAIRING_CHAT_ID).expect("sentinel entry");
        assert_eq!(entry.0, code);
    }

    /// #334: A `/pair <code>` from any chat can claim the sentinel entry.
    ///
    /// Why: This is the core security guarantee — the REPL issues the code,
    /// any Telegram chat can validate against it. We verify the
    /// `verify_pair_attempt` lookup against the sentinel returns Success.
    /// Test: Issue code, then verify the same code against the sentinel entry.
    #[tokio::test]
    async fn repl_issued_code_promotes_chat_via_sentinel() {
        let pending = new_pending_pairs();
        let code = issue_repl_pairing_code(&pending).await;

        let now = Instant::now();
        let map = pending.lock().await;
        let outcome = verify_pair_attempt(
            map.get(&SENTINEL_PAIRING_CHAT_ID),
            &code,
            now,
            PAIRING_CODE_TTL,
        );
        assert_eq!(outcome, PairOutcome::Success);
    }

    /// #334: Sentinel entry past TTL returns Expired.
    ///
    /// Why: TTL handling for sentinel entries must match per-chat entries.
    /// Test: Build a synthetic entry with `issued` in the past and assert
    /// `Expired`.
    #[test]
    fn sentinel_expired_code_is_rejected() {
        let issued = Instant::now();
        let entry = ("123456".to_string(), issued);
        let now = issued + PAIRING_CODE_TTL + Duration::from_secs(1);
        let outcome = verify_pair_attempt(Some(&entry), "123456", now, PAIRING_CODE_TTL);
        assert_eq!(outcome, PairOutcome::Expired);
    }

    /// #334: With nothing under the sentinel, lookup returns NoPending.
    ///
    /// Why: A `/pair` arriving before the REPL has issued any code must be
    /// rejected with NoPending so the user is told to run /telegram pair.
    /// Test: Empty map -> sentinel lookup -> NoPending.
    #[tokio::test]
    async fn empty_pending_map_returns_no_pending() {
        let pending = new_pending_pairs();
        let map = pending.lock().await;
        let outcome = verify_pair_attempt(
            map.get(&SENTINEL_PAIRING_CHAT_ID),
            "123456",
            Instant::now(),
            PAIRING_CODE_TTL,
        );
        assert_eq!(outcome, PairOutcome::NoPending);
    }

    /// #467: Round-trip a `PairedChats` map through disk to verify
    /// `save_paired_chats` + `load_paired_chats` preserve chat ids.
    ///
    /// Why: Regression guard for the pairing-persistence feature. Without
    /// this, a serializer or path-handling regression would silently break
    /// every user's pairing on the next upgrade.
    /// What: Insert two chats, save, load into a fresh map, verify both
    /// chat ids survived.
    /// Test: This is the test.
    #[tokio::test]
    async fn paired_state_round_trip() {
        let tmp = tempdir_for_test();
        let path = tmp.join("telegram-paired.json");
        let paired: PairedChats = Arc::new(RwLock::new(HashMap::new()));
        {
            let mut g = paired.write().await;
            g.insert(ChatId(111), Instant::now());
            g.insert(ChatId(222), Instant::now());
        }
        save_paired_chats(&paired, &path)
            .await
            .expect("save should succeed");
        let loaded = load_paired_chats(&path).await;
        let g = loaded.read().await;
        assert!(g.contains_key(&ChatId(111)));
        assert!(g.contains_key(&ChatId(222)));
        assert_eq!(g.len(), 2);
    }

    /// #467: Missing state file is treated as "first run", not an error.
    #[tokio::test]
    async fn paired_state_missing_file_is_empty() {
        let tmp = tempdir_for_test();
        let path = tmp.join("does-not-exist.json");
        let loaded = load_paired_chats(&path).await;
        assert!(loaded.read().await.is_empty());
    }

    /// #467: A malformed JSON file must not panic; we fail open with empty.
    #[tokio::test]
    async fn paired_state_malformed_file_is_empty() {
        let tmp = tempdir_for_test();
        let path = tmp.join("broken.json");
        tokio::fs::write(&path, b"{not json").await.unwrap();
        let loaded = load_paired_chats(&path).await;
        assert!(loaded.read().await.is_empty());
    }

    /// Single-instance guard: our own PID must report as alive.
    #[test]
    fn telegram_pid_alive_true_for_self() {
        let self_pid = std::process::id() as i32;
        assert!(telegram_pid_alive(self_pid));
    }

    /// Single-instance guard: an implausible PID must report as dead.
    ///
    /// Why: `acquire` relies on this to distinguish a live peer from a stale
    /// lock. PID 0x7FFF_FFFF is far beyond any real PID, so `kill(pid, 0)`
    /// fails with ESRCH.
    #[test]
    fn telegram_pid_alive_false_for_absurd_pid() {
        assert!(!telegram_pid_alive(i32::MAX));
    }

    /// Single-instance guard: `acquire` writes the current PID and `Drop`
    /// removes the file.
    #[test]
    fn telegram_pid_guard_acquire_writes_and_drops() {
        let tmp = tempdir_for_test();
        let path = tmp.join("telegram.pid");
        {
            let _guard = TelegramPidGuard::acquire(path.clone()).expect("acquire");
            let contents = std::fs::read_to_string(&path).expect("pid file exists");
            assert_eq!(contents.trim(), std::process::id().to_string());
        }
        // Guard dropped: file must be gone.
        assert!(!path.exists(), "PID file should be removed on drop");
    }

    /// Single-instance guard: a stale PID file (dead process) is overwritten,
    /// not treated as a live conflict.
    #[test]
    fn telegram_pid_guard_stale_is_overwritten() {
        let tmp = tempdir_for_test();
        let path = tmp.join("telegram.pid");
        // Write an absurd, definitely-dead PID.
        std::fs::write(&path, i32::MAX.to_string()).unwrap();
        let _guard =
            TelegramPidGuard::acquire(path.clone()).expect("stale lock should be reclaimed");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim(), std::process::id().to_string());
    }

    /// Single-instance guard: a live PID file (our own PID) blocks acquire.
    ///
    /// Why: This is the core protection — a second daemon must refuse to
    /// start while a peer is alive. We use our own PID as a stand-in for a
    /// running peer since it is guaranteed alive for the test's duration.
    #[test]
    fn telegram_pid_guard_live_conflict_is_rejected() {
        let tmp = tempdir_for_test();
        let path = tmp.join("telegram.pid");
        std::fs::write(&path, std::process::id().to_string()).unwrap();
        let result = TelegramPidGuard::acquire(path.clone());
        assert!(result.is_err(), "live peer must block acquire");
        // The pre-existing file must be left intact for the live peer.
        assert!(path.exists());
    }

    /// Single-instance guard: unparseable PID file contents are treated as
    /// stale and overwritten.
    #[test]
    fn telegram_pid_guard_garbage_is_overwritten() {
        let tmp = tempdir_for_test();
        let path = tmp.join("telegram.pid");
        std::fs::write(&path, "not-a-pid").unwrap();
        let _guard =
            TelegramPidGuard::acquire(path.clone()).expect("garbage lock should be reclaimed");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim(), std::process::id().to_string());
    }

    /// Tiny helper that creates a unique tempdir under the system temp.
    /// Why: Avoids pulling in the `tempfile` crate just for two tests.
    fn tempdir_for_test() -> PathBuf {
        let uniq = format!(
            "open-mpm-telegram-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(uniq);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
