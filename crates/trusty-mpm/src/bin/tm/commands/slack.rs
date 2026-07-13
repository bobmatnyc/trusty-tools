//! Slack bot command handlers (DOC-20 adapter, #1294).
//!
//! Why: the Slack operations live under one discoverable `tm slack <subcommand>`
//! group, exactly mirroring `tm telegram`. The Slack adapter is a thin peer of the
//! Telegram adapter over the SAME chat-core nucleus, so its CLI surface mirrors
//! Telegram's `start` lifecycle (pair/status are intentionally NOT duplicated —
//! Slack uses its own app-install pairing, not the daemon pairing store).
//! What: the `slack` dispatcher and the `slack_start` / `slack_stop` handlers.
//! `start` runs the Socket-Mode bot in the foreground (recording its PID at
//! `~/.trusty-mpm/slack.pid`); `stop` signals exactly that PID.
//! Test: `cli_parses_slack_*` in `tests.rs`; the handlers are exercised against a
//! live Slack app (deferred — needs an installed Slack app, see the PR body).

use crate::cli::SlackCmd;

/// `slack` subcommand — manage the Slack bot (start, stop).
///
/// Why: every Slack operation belongs under one discoverable command group; this
/// dispatcher routes the [`SlackCmd`] variants to their handlers, resolving the
/// bot/app tokens from flags or the dotenv/env fallback (mirroring how
/// `tm telegram start` resolves `TELEGRAM_BOT_TOKEN`).
/// What: `Start` runs the Socket-Mode bot in the foreground; `Stop` signals the
/// PID recorded in `~/.trusty-mpm/slack.pid`.
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
            // #2487: `start_url` is `Option<String>` (no clap `default_value`)
            // — resolve through the shared `resolve_daemon_url` so an unset
            // `--url`/`TRUSTY_MPM_URL` still finds a daemon on an ephemeral
            // port via the lock file, matching every other command family.
            let resolved_url = trusty_mpm::core::resolve_daemon_url(start_url.as_deref());
            let bot_token =
                bot_token.or_else(|| trusty_mpm::slack::resolve_token("SLACK_BOT_TOKEN"));
            let app_token =
                app_token.or_else(|| trusty_mpm::slack::resolve_token("SLACK_APP_TOKEN"));
            trusty_mpm::slack::run(resolved_url, bot_token, app_token, check).await
        }
        SlackCmd::Stop => slack_stop(),
    }
}

/// `slack stop` — stop the foreground Slack bot recorded in its PID file.
///
/// Why: the bot runs standalone in the foreground (`tm slack start`, which records
/// its PID at `~/.trusty-mpm/slack.pid`); `tm slack stop` takes it down precisely.
/// This deliberately does NOT mirror `tm telegram stop`'s `pkill -f "telegram
/// start"`: a broad `pkill -f "slack start"` is fragile (it can match and kill an
/// unrelated process whose argv merely contains that string — an editor, a grep,
/// another tool) and conflates "no match" with "pkill errored". The PID-file
/// approach signals exactly the process we launched and reports honestly.
/// What: delegates to [`trusty_mpm::slack::stop_via_pid_file`], which reads
/// `~/.trusty-mpm/slack.pid`, sends `SIGTERM` to that pid, and removes the file;
/// prints the precise outcome (stopped / not running / failed).
/// Test: the stop logic is unit-tested in `trusty_mpm::slack`
/// (`stop_via_pid_file_*`); this thin wrapper is exercised manually.
fn slack_stop() -> anyhow::Result<()> {
    use trusty_mpm::slack::{StopOutcome, pid_file_path, stop_via_pid_file};
    match stop_via_pid_file(&pid_file_path()) {
        StopOutcome::Stopped(pid) => println!("Slack bot stopped (pid {pid})"),
        StopOutcome::NotRunning => println!("no Slack bot process found"),
        StopOutcome::Failed(msg) => eprintln!("failed to stop Slack bot: {msg}"),
    }
    Ok(())
}
