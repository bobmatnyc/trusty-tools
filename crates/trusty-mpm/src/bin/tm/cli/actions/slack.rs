//! `tm slack` remote-management bot command group (DOC-20, #1294).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`SlackCmd`] — `start`/`stop`.
//! Test: `cli_parses_slack_*` in `tests.rs`.

use clap::Subcommand;

/// Actions for the `slack` subcommand (DOC-20 adapter, #1294).
///
/// Why: the Slack bot mirrors the Telegram bot's lifecycle surface over the SAME
/// chat-core nucleus. Pairing is NOT duplicated — Slack uses its own app-install
/// flow (a bot token + app-level token), not the daemon pairing store — so only
/// the `start`/`stop` lifecycle is exposed here.
/// What: `Start` runs the Socket-Mode bot in the foreground; `Stop` kills it.
/// Test: `cli_parses_slack_start`, `cli_parses_slack_stop`.
#[derive(Debug, Subcommand)]
pub(crate) enum SlackCmd {
    /// Start the Slack bot process (Socket Mode — no public webhook required).
    Start {
        /// Base URL of the trusty-mpm daemon. `Option<String>`, no
        /// `default_value` (#2487) — resolved via `resolve_daemon_url`, which
        /// applies the lock-file / compiled-in-default fallback itself.
        #[arg(long, env = "TRUSTY_MPM_URL")]
        url: Option<String>,
        /// Slack bot token (`xoxb-…`). When omitted, resolved from `.env.local` /
        /// `.env` / the `SLACK_BOT_TOKEN` environment variable.
        #[arg(long)]
        bot_token: Option<String>,
        /// Slack app-level token (`xapp-…`, Socket Mode). When omitted, resolved
        /// from `.env.local` / `.env` / the `SLACK_APP_TOKEN` environment variable.
        #[arg(long)]
        app_token: Option<String>,
        /// Validate configuration and exit without connecting to Slack.
        #[arg(long)]
        check: bool,
    },
    /// Stop the Slack bot process if running.
    Stop,
}
