//! `tm telegram` remote-management bot command group.
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`TelegramCmd`] — `pair`/`status`/`start`/`stop`.
//! Test: `cli_parses_telegram_*` in `tests.rs`.

use clap::Subcommand;

/// Actions for the `telegram` subcommand.
///
/// Why: every Telegram-related operation — pairing, status, and lifecycle —
/// now lives under one `tm telegram <subcommand>` group instead of scattered
/// top-level commands, so the bot's controls are discoverable in one place.
/// What: `Pair` requests a one-time pairing code; `Status` reports the daemon's
/// paired/unpaired state; `Start` runs the bot process in the foreground;
/// `Stop` kills a running bot process.
/// Test: `cli_parses_telegram_pair`, `cli_parses_telegram_status`,
/// `cli_parses_telegram_start`, `cli_parses_telegram_stop`.
#[derive(Debug, Subcommand)]
pub(crate) enum TelegramCmd {
    /// Request a one-time pairing code for the Telegram bot.
    Pair,
    /// Show Telegram bot pairing status.
    Status,
    /// Start the Telegram bot process.
    Start {
        /// Base URL of the trusty-mpm daemon. `Option<String>`, no
        /// `default_value` (#2487) — resolved via `resolve_daemon_url`, which
        /// applies the lock-file / compiled-in-default fallback itself.
        #[arg(long, env = "TRUSTY_MPM_URL")]
        url: Option<String>,
        /// Telegram bot token. When omitted, resolved from `.env.local` /
        /// `.env` / the `TELEGRAM_BOT_TOKEN` environment variable.
        #[arg(long)]
        token: Option<String>,
        /// Chat id to push unsolicited alerts to.
        #[arg(long)]
        alert_chat_id: Option<i64>,
        /// Restrict the bot to this Telegram user id.
        #[arg(long)]
        allowed_user_id: Option<i64>,
        /// Validate configuration and exit without connecting to Telegram.
        #[arg(long)]
        check: bool,
    },
    /// Stop the Telegram bot process if running.
    Stop,
}
