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
    /// A plain user message (free text) routed to the action-capable coordinator.
    Message {
        /// Socket-Mode envelope id to ACK.
        envelope_id: String,
        /// The message text.
        text: String,
        /// The channel id the reply should be posted to.
        channel: String,
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

/// The PID-file name (under `~/.trusty-mpm`) for the foreground Slack bot.
///
/// Why: `tm slack stop` must terminate exactly the bot `tm slack start` launched —
/// not any process whose argv happens to contain "slack start" (the old
/// `pkill -f "slack start"` could kill an editor, a grep, or an unrelated tool).
/// A PID file records the one true process so `stop` signals precisely it.
const SLACK_PID_FILE: &str = "slack.pid";

/// Absolute path to the Slack bot PID file (`~/.trusty-mpm/slack.pid`).
///
/// Why: `start` (writer) and `stop` (reader) must agree on one location; deriving
/// it from the shared [`FRAMEWORK_DIR_NAME`](crate::core::paths::FRAMEWORK_DIR_NAME)
/// home root keeps it consistent with the rest of the framework's state files.
/// What: `~/.trusty-mpm/slack.pid`, falling back to `./.trusty-mpm/slack.pid` when
/// the home directory cannot be resolved (mirrors `FrameworkPaths::default`).
/// Test: `pid_file_path_is_under_framework_root`.
pub fn pid_file_path() -> std::path::PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(crate::core::paths::FRAMEWORK_DIR_NAME)
        .join(SLACK_PID_FILE)
}

/// Record the current process id in the Slack PID file.
///
/// Why: so `tm slack stop` can signal exactly this foreground bot instead of
/// pattern-matching process argv. Written once at startup, removed on stop.
/// What: creates `~/.trusty-mpm` if needed and writes the current PID as text.
/// A write failure is logged (stderr) and swallowed — an unwritable PID file must
/// not prevent the bot from running; `stop` simply falls back to a manual kill.
/// Test: `write_then_read_pid_file_round_trips` (against a temp path).
fn write_pid_file() {
    let path = pid_file_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("could not create Slack PID dir {}: {e}", parent.display());
        return;
    }
    if let Err(e) = std::fs::write(&path, std::process::id().to_string()) {
        tracing::warn!("could not write Slack PID file {}: {e}", path.display());
    }
}

/// RAII guard that best-effort removes the Slack PID file when dropped.
///
/// Why: [`run`] writes the PID file at startup so `tm slack stop` can signal
/// exactly this process, but a clean exit (e.g. a permanent `disconnect`), an
/// early `?` return, or a panic would otherwise leave a STALE PID file behind —
/// a later `tm slack stop` could then SIGTERM a recycled, unrelated PID. Holding
/// the path in a `Drop` guard removes the file on EVERY exit path of `run`.
/// What: on drop, removes the file at `path`. Best-effort: a `NotFound` is
/// expected (e.g. `tm slack stop` already removed it) and silent; any other
/// removal error is logged to stderr and swallowed — cleanup must never panic.
/// Test: `pid_file_guard_removes_on_drop`, `pid_file_guard_drop_missing_is_silent`.
struct PidFileGuard {
    /// The PID-file path to remove on drop.
    path: std::path::PathBuf,
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    "could not remove Slack PID file {} on exit: {e}",
                    self.path.display()
                );
            }
        }
    }
}

/// The result of a `tm slack stop` attempt, for a precise operator message.
///
/// Why: `stop` must report honestly — it should NOT print "no process found" when
/// it actually hit an error (an unreadable PID file or a failed signal). The old
/// `pkill` path conflated "no match" with "pkill itself errored"; a typed outcome
/// keeps the three cases distinct.
/// What: `Stopped(pid)` signalled a live bot; `NotRunning` found no PID file or a
/// stale one (process already gone); `Failed(msg)` is a real error.
/// Test: `stop_via_pid_file_*` cover the missing-file and stale-pid paths.
#[derive(Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// A running bot (this PID) was signalled to stop.
    Stopped(u32),
    /// No bot was running (no PID file, or the recorded PID is already gone).
    NotRunning,
    /// Stopping failed for a real reason (e.g. an unreadable PID file).
    Failed(String),
}

/// Stop the foreground Slack bot recorded in the PID file at `path`.
///
/// Why: this is the precise replacement for `pkill -f "slack start"` — it signals
/// exactly the process `tm slack start` recorded, so it can never kill an
/// unrelated process, and it distinguishes "not running" from a genuine failure.
/// Taking `path` as a parameter keeps it unit-testable against a temp file.
/// What: reads the PID, sends `SIGTERM` to it (Unix), removes the PID file, and
/// returns a [`StopOutcome`]. A missing PID file → `NotRunning`; a recorded PID
/// that no longer exists → `NotRunning` (stale file, still cleaned up); an
/// unreadable/garbage PID file or a signal error other than "no such process" →
/// `Failed`.
/// Test: `stop_via_pid_file_missing_is_not_running`,
/// `stop_via_pid_file_stale_pid_is_not_running`.
pub fn stop_via_pid_file(path: &Path) -> StopOutcome {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return StopOutcome::NotRunning,
        Err(e) => return StopOutcome::Failed(format!("could not read PID file: {e}")),
    };
    let pid: u32 = match raw.trim().parse() {
        Ok(pid) => pid,
        Err(e) => {
            // A corrupt PID file is unusable; remove it so the next start is clean.
            let _ = std::fs::remove_file(path);
            return StopOutcome::Failed(format!("PID file is not a valid pid: {e}"));
        }
    };
    let outcome = signal_terminate(pid);
    // Always clear the PID file once we have acted on it (stopped or stale).
    let _ = std::fs::remove_file(path);
    outcome
}

/// Send `SIGTERM` to `pid`, classifying the result.
///
/// Why: `stop_via_pid_file` must distinguish a successful signal from "the process
/// is already gone" (a stale PID file, not an error) and from a real failure.
/// Isolating the raw `kill(2)` call keeps that classification in one place.
/// What: on Unix, calls `libc::kill(pid, SIGTERM)`; `ESRCH` (no such process) maps
/// to [`StopOutcome::NotRunning`], any other errno to [`StopOutcome::Failed`], and
/// success to [`StopOutcome::Stopped`]. On non-Unix it reports unsupported.
/// Test: covered indirectly via `stop_via_pid_file_stale_pid_is_not_running`.
fn signal_terminate(pid: u32) -> StopOutcome {
    #[cfg(unix)]
    {
        // SAFETY: `kill` is async-signal-safe and merely posts a signal; passing a
        // pid and a constant signal number has no memory-safety implications.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            return StopOutcome::Stopped(pid);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // The process is already gone — a stale PID file, not a failure.
            StopOutcome::NotRunning
        } else {
            StopOutcome::Failed(format!("failed to signal pid {pid}: {err}"))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        StopOutcome::Failed("stopping the Slack bot is only supported on Unix".to_string())
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
        match socket_loop(&http, &bot_token, &ws_url, &executor, &histories).await {
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
        if let Some((channel, body)) = reply_for_event(executor, histories, &event).await {
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
