//! trusty-mpm Telegram bot shim (`trusty-mpm-telegram`).
//!
//! Why: kept as a backward-compatible standalone binary — the primary entry
//! point is now `trusty-mpm telegram`, which calls the same
//! [`trusty_mpm::telegram::run`].
//! What: parses CLI flags and delegates to the library's `run`. The bot token
//! is resolved from `.env.local` / `.env` / the environment when not given.
//! Test: `cargo run -p trusty-mpm-telegram -- --check` validates config.

use clap::Parser;

use trusty_mpm::telegram::BotOptions;

/// trusty-mpm Telegram bot command-line options.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-mpm-telegram",
    version,
    about = "trusty-mpm Telegram bot"
)]
struct Args {
    /// Base URL of the trusty-mpm daemon.
    #[arg(long, env = "TRUSTY_MPM_URL", default_value = "http://127.0.0.1:7880")]
    url: String,

    /// Telegram bot token. When omitted, resolved from `.env.local` / `.env` /
    /// the `TELEGRAM_BOT_TOKEN` environment variable.
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let token = args
        .token
        .or_else(|| trusty_mpm::telegram::resolve_token("TELEGRAM_BOT_TOKEN"));
    let options = BotOptions {
        allowed_user_id: args.allowed_user_id,
        alert_chat_id: args.alert_chat_id,
    };
    trusty_mpm::telegram::run(args.url, token, args.check, options).await
}
