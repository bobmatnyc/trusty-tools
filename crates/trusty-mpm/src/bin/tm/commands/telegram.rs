//! Telegram bot command handlers.
//!
//! Why: the four Telegram operations (pair, status, start, stop) live under one
//! discoverable group and benefit from a dedicated file.
//! What: `telegram` dispatcher, `telegram_pair`, `telegram_status`,
//! `telegram_stop`.
//! Test: `cli_parses_telegram_*` in `tests.rs`; handlers are exercised against
//! a live daemon.

use crate::cli::TelegramCmd;

/// `telegram` subcommand — manage the Telegram bot (pair, status, start, stop).
///
/// Why: every Telegram operation belongs under one discoverable command group;
/// this dispatcher routes the [`TelegramCmd`] variants to their handlers.
/// What: `Pair` requests a pairing code, `Status` queries the daemon's pairing
/// state, `Start` runs the bot process in the foreground, `Stop` kills it.
/// Test: variant routing is covered by the `cli_parses_telegram_*` tests; the
/// handlers themselves are exercised against a live daemon.
pub(crate) async fn telegram(url: &str, cmd: TelegramCmd) -> anyhow::Result<()> {
    match cmd {
        TelegramCmd::Pair => telegram_pair(url).await,
        TelegramCmd::Status => telegram_status(url).await,
        TelegramCmd::Start {
            url: start_url,
            token,
            alert_chat_id,
            allowed_user_id,
            check,
        } => {
            let token = token.or_else(|| trusty_mpm::telegram::resolve_token("TELEGRAM_BOT_TOKEN"));
            let options = trusty_mpm::telegram::BotOptions {
                allowed_user_id,
                alert_chat_id,
            };
            trusty_mpm::telegram::run(start_url, token, check, options).await
        }
        TelegramCmd::Stop => telegram_stop(),
    }
}

/// `telegram pair` — request a Telegram-bot pairing code.
///
/// Why: pairing the Telegram bot to this daemon needs an out-of-band shared
/// secret; `tm telegram pair` asks the local daemon for a short code and prints
/// both the `/pair` command and a `t.me` deep link the operator can use.
/// What: calls `POST /pair/request` via the shared [`CommandExecutor`] and
/// prints the code, its TTL, the `/pair <code>` command, and the deep link.
/// Test: covered by the executor's `pair_request_returns_code` test.
async fn telegram_pair(url: &str) -> anyhow::Result<()> {
    use trusty_mpm::client::{CommandExecutor, CommandResult};
    let executor = CommandExecutor::new(url.to_string());
    match executor.pair_request().await {
        CommandResult::PairCode {
            code,
            expires_in_seconds,
        } => {
            println!("Pairing code: {code}");
            println!("Expires in: {} minutes", expires_in_seconds / 60);
            println!();
            println!("In Telegram, send to your bot:");
            println!("  /pair {code}");
            println!();
            println!("Or click: https://t.me/trusty_mpm_bot?start={code}");
        }
        CommandResult::Error(msg) => eprintln!("pairing failed: {msg}"),
        other => eprintln!("unexpected pairing result: {other:?}"),
    }
    Ok(())
}

/// `telegram status` — show the daemon's Telegram pairing state.
///
/// Why: operators need to know whether a Telegram chat is already paired with
/// the daemon — and which one — without digging through logs.
/// What: calls `GET /pair/status` via the shared [`DaemonClient`] and prints
/// `paired` plus the registered `chat_id`, or `unpaired` when no chat is bound.
/// Test: covered by the client's `pair_status_deserializes` test.
async fn telegram_status(url: &str) -> anyhow::Result<()> {
    use trusty_mpm::client::DaemonClient;
    let client = DaemonClient::new(url.to_string());
    match client.pair_status().await {
        Ok(status) if status.paired => match status.chat_id {
            Some(chat_id) => println!("Telegram: paired (chat_id: {chat_id})"),
            None => println!("Telegram: paired"),
        },
        Ok(_) => println!("Telegram: unpaired — run `tm telegram pair` to begin"),
        Err(e) => eprintln!("daemon unreachable: {e}"),
    }
    Ok(())
}

/// `telegram stop` — kill the Telegram bot process if one is running.
///
/// Why: the bot may be running standalone (`tm telegram start`) or alongside
/// the daemon; `tm telegram stop` gives the operator a single way to take it
/// down without hunting for the PID.
/// What: runs `pkill -f "trusty-mpm telegram start"` to terminate any
/// foreground bot process; reports whether a process was killed.
/// Test: exercised manually — process management is not unit-testable here.
fn telegram_stop() -> anyhow::Result<()> {
    let status = std::process::Command::new("pkill")
        .args(["-f", "telegram start"])
        .status();
    match status {
        Ok(s) if s.success() => println!("Telegram bot stopped"),
        Ok(_) => println!("no Telegram bot process found"),
        Err(e) => eprintln!("failed to stop Telegram bot: {e}"),
    }
    Ok(())
}
