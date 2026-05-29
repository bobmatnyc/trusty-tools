//! Part of the `commands` module (split from the monolithic `commands.rs`
//! for the 500-line file cap — see #357). Holds an `impl OpenMpmRepl` block
//! for one slash-command handler group.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::repl::OpenMpmRepl;

impl OpenMpmRepl {
    /// Handle `/telegram [start|stop|status|pair]` slash command.
    pub(crate) async fn handle_telegram_command_into(&mut self, arg: &str, out: &mut String) {
        match arg {
            "pair" => {
                // #334: Generate the code in the REPL (trusted side). The
                // Telegram bot only validates — it never generates.
                let code = crate::telegram::issue_repl_pairing_code(&self.telegram_pairing).await;
                let _ = writeln!(out, "Telegram pairing code: {code}");
                let _ = writeln!(
                    out,
                    "Expires in 5 minutes. In Telegram, send:  /pair {code}"
                );
                let bot_running = self
                    .telegram_handle
                    .as_ref()
                    .map(|h| !h.is_finished())
                    .unwrap_or(false);
                if !bot_running {
                    let _ = writeln!(
                        out,
                        "Note: Telegram bot is not running. Start it with /telegram start."
                    );
                }
            }
            "stop" => {
                if let Some(h) = self.telegram_handle.take() {
                    h.abort();
                    let _ = writeln!(out, "Telegram bot stopped.");
                } else {
                    let _ = writeln!(out, "Telegram bot is not running.");
                }
            }
            "status" => {
                let running = self
                    .telegram_handle
                    .as_ref()
                    .map(|h| !h.is_finished())
                    .unwrap_or(false);
                let token_ok = std::env::var("TELEGRAM_BOT_TOKEN").is_ok();
                let _ = writeln!(
                    out,
                    "Telegram bot: {}",
                    if running { "running" } else { "stopped" }
                );
                let _ = writeln!(
                    out,
                    "TELEGRAM_BOT_TOKEN: {}",
                    if token_ok { "set" } else { "NOT SET" }
                );
            }
            "" | "start" => {
                if let Some(ref h) = self.telegram_handle
                    && !h.is_finished()
                {
                    let _ = writeln!(
                        out,
                        "Telegram bot is already running. Use /telegram stop to stop it."
                    );
                    return;
                }
                if std::env::var("TELEGRAM_BOT_TOKEN").is_err() {
                    let _ = writeln!(
                        out,
                        "TELEGRAM_BOT_TOKEN not set. Add it to .env.local before starting the bot."
                    );
                    return;
                }
                let project_path = self.project_dir.clone();
                let pending = self.telegram_pairing.clone();
                let handle = tokio::spawn(async move {
                    if let Err(e) = crate::telegram::run_telegram_bot(project_path, pending).await {
                        tracing::error!("Telegram bot error: {e:#}");
                    }
                });
                self.telegram_handle = Some(handle);
                let _ = writeln!(
                    out,
                    "Telegram bot started (@openmpm_bot). Use /telegram stop to stop it."
                );
            }
            other => {
                let _ = writeln!(out, "unknown telegram subcommand: {other}");
                let _ = writeln!(out, "usage: /telegram [start|stop|status|pair]");
            }
        }
    }

    /// Handle `/slack [start|stop|status|pair]` slash command (#452).
    ///
    /// Why: Lets users start/stop the Slack Socket Mode bot and mint pairing
    /// codes without restarting the harness. Mirrors `/telegram` exactly so
    /// the two adapters expose a uniform operator surface.
    /// What: `start` spawns `run_slack_bot` on a background task; `stop`
    /// aborts it; `status` reports running state + token presence; `pair`
    /// generates a one-time code stored under the sentinel key in the shared
    /// `PendingPairs` map.
    /// Test: Manual via `/slack start`, `/slack status`, `/slack pair`,
    /// `/slack stop` in the REPL. Unit-tested pieces live in `src/slack/`.
    pub(crate) async fn handle_slack_command_into(&mut self, arg: &str, out: &mut String) {
        match arg {
            "pair" => {
                // #452: Generate the code in the REPL (trusted side). The
                // Slack bot only validates — it never generates.
                let code = crate::slack::issue_repl_pairing_code(&self.slack_pairing).await;
                let _ = writeln!(out, "Slack pairing code: {code}");
                let _ = writeln!(
                    out,
                    "Expires in 5 minutes. In Slack, send:  /slack-pair {code}"
                );
                let bot_running = self
                    .slack_handle
                    .as_ref()
                    .map(|h| !h.is_finished())
                    .unwrap_or(false);
                if !bot_running {
                    let _ = writeln!(
                        out,
                        "Note: Slack bot is not running. Start it with /slack start."
                    );
                }
            }
            "stop" => {
                if let Some(h) = self.slack_handle.take() {
                    h.abort();
                    let _ = writeln!(out, "Slack bot stopped.");
                } else {
                    let _ = writeln!(out, "Slack bot is not running.");
                }
            }
            "status" => {
                let running = self
                    .slack_handle
                    .as_ref()
                    .map(|h| !h.is_finished())
                    .unwrap_or(false);
                let app_token_ok = std::env::var("SLACK_APP_TOKEN").is_ok();
                let bot_token_ok = std::env::var("SLACK_BOT_TOKEN").is_ok();
                let _ = writeln!(
                    out,
                    "Slack bot: {}",
                    if running { "running" } else { "stopped" }
                );
                let _ = writeln!(
                    out,
                    "SLACK_APP_TOKEN: {}",
                    if app_token_ok { "set" } else { "NOT SET" }
                );
                let _ = writeln!(
                    out,
                    "SLACK_BOT_TOKEN: {}",
                    if bot_token_ok { "set" } else { "NOT SET" }
                );
            }
            "" | "start" => {
                if let Some(ref h) = self.slack_handle
                    && !h.is_finished()
                {
                    let _ = writeln!(
                        out,
                        "Slack bot is already running. Use /slack stop to stop it."
                    );
                    return;
                }
                if std::env::var("SLACK_APP_TOKEN").is_err() {
                    let _ = writeln!(
                        out,
                        "SLACK_APP_TOKEN not set. Add it to .env.local before starting the bot."
                    );
                    return;
                }
                if std::env::var("SLACK_BOT_TOKEN").is_err() {
                    let _ = writeln!(
                        out,
                        "SLACK_BOT_TOKEN not set. Add it to .env.local before starting the bot."
                    );
                    return;
                }
                let project_path = self.project_dir.clone();
                let pending = self.slack_pairing.clone();
                // #480/#481: Parse the per-user RBAC table + default persona
                // from env so a REPL-started bot enforces the same access
                // tiers as a `--slack`-launched one.
                let rbac = std::sync::Arc::new(crate::slack::SlackRbacConfig::from_env());
                let handle = tokio::spawn(async move {
                    if let Err(e) = crate::slack::run_slack_bot(project_path, pending, rbac).await {
                        tracing::error!("Slack bot error: {e:#}");
                    }
                });
                self.slack_handle = Some(handle);
                let _ = writeln!(out, "Slack bot started. Use /slack stop to stop it.");
            }
            other => {
                let _ = writeln!(out, "unknown slack subcommand: {other}");
                let _ = writeln!(out, "usage: /slack [start|stop|status|pair]");
            }
        }
    }

    /// Run the memories search subprocess and capture both stdout and stderr
    /// into `out`. Why: in ratatui mode we cannot let the subprocess inherit
    /// the parent's stdout/stderr — its writes would corrupt the alt-screen
    /// buffer just like a stray `println!`.
    pub(crate) async fn run_memories_into(&self, query: &str, out: &mut String) {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("open-mpm"));
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("memories").arg("search");
        if !query.is_empty() {
            cmd.arg(query);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        match cmd.output().await {
            Ok(output) => {
                if !output.stdout.is_empty() {
                    out.push_str(&String::from_utf8_lossy(&output.stdout));
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
                if !output.stderr.is_empty() {
                    out.push_str(&String::from_utf8_lossy(&output.stderr));
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
                if !output.status.success() {
                    let _ = writeln!(out, "memories search exited with {}", output.status);
                }
            }
            Err(e) => {
                let _ = writeln!(out, "failed to run memories search: {e}");
            }
        }
    }
}
