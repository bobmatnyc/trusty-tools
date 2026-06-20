//! Slack bot command handlers (DOC-20 adapter, #1294).
//!
//! Why: the Slack operations live under one discoverable `tm slack <subcommand>`
//! group, exactly mirroring `tm telegram`. The Slack adapter is a thin peer of the
//! Telegram adapter over the SAME chat-core nucleus, so its CLI surface mirrors
//! Telegram's `start` lifecycle (pair/status are intentionally NOT duplicated —
//! Slack uses its own app-install pairing, not the daemon pairing store).
//! What: the `slack` dispatcher and the `slack_start` / `slack_stop` handlers.
//! `start` runs the Socket-Mode bot in the foreground; `stop` kills it.
//! Test: `cli_parses_slack_*` in `tests.rs`; the handlers are exercised against a
//! live Slack app (deferred — needs an installed Slack app, see the PR body).

use crate::cli::SlackCmd;

/// `slack` subcommand — manage the Slack bot (start, stop).
///
/// Why: every Slack operation belongs under one discoverable command group; this
/// dispatcher routes the [`SlackCmd`] variants to their handlers, resolving the
/// bot/app tokens from flags or the dotenv/env fallback (mirroring how
/// `tm telegram start` resolves `TELEGRAM_BOT_TOKEN`).
/// What: `Start` runs the Socket-Mode bot in the foreground; `Stop` kills it.
/// Test: variant routing is covered by the `cli_parses_slack_*` tests; the
/// handlers themselves are exercised against a live Slack app.
pub(crate) async fn slack(cmd: SlackCmd) -> anyhow::Result<()> {
    match cmd {
        SlackCmd::Start {
            url: start_url,
            bot_token,
            app_token,
            check,
        } => {
            let bot_token =
                bot_token.or_else(|| trusty_mpm::slack::resolve_token("SLACK_BOT_TOKEN"));
            let app_token =
                app_token.or_else(|| trusty_mpm::slack::resolve_token("SLACK_APP_TOKEN"));
            trusty_mpm::slack::run(start_url, bot_token, app_token, check).await
        }
        SlackCmd::Stop => slack_stop(),
    }
}

/// `slack stop` — kill the Slack bot process if one is running.
///
/// Why: the bot runs standalone in the foreground (`tm slack start`); `tm slack
/// stop` gives the operator one way to take it down without hunting for the PID
/// (mirrors `tm telegram stop`).
/// What: runs `pkill -f "slack start"` to terminate any foreground bot process;
/// reports whether a process was killed.
/// Test: exercised manually — process management is not unit-testable here.
fn slack_stop() -> anyhow::Result<()> {
    let status = std::process::Command::new("pkill")
        .args(["-f", "slack start"])
        .status();
    match status {
        Ok(s) if s.success() => println!("Slack bot stopped"),
        Ok(_) => println!("no Slack bot process found"),
        Err(e) => eprintln!("failed to stop Slack bot: {e}"),
    }
    Ok(())
}
