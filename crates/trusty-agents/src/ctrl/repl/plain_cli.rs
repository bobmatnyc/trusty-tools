//! Persistent plain-line CLI mode (#3052) — split out of `repl/mod.rs` to
//! keep that file under the 500-SLOC production cap.
//!
//! Why: `run_ctrl`'s own stdin loop (in `mod.rs`) dispatches free text
//! through `ctrl_chat_turn` / `Ctrl::dispatch_task` — a DIFFERENT pipeline
//! from what the ratatui TUI's submit path uses
//! (`TrustyAgentsRepl::attempt_forward`, which routes through
//! `run_pm_task_with_persona` / `run_pm_task_with_history` with `/model` +
//! `/provider` session overrides applied). A tester driving `tagent` over
//! SSH (Terminus/iPhone) needs the SAME agent/model routing the TUI gets,
//! not a parallel implementation, and needs NO full-screen alt-screen TUI
//! (which reflows badly on a narrow terminal and has a modal
//! keystroke-swallow bug). This module reuses `TrustyAgentsRepl` directly —
//! `try_handle_slash` for every slash command (`/help`, `/model`,
//! `/provider`, `/switch`, `/clear`, `/quit`, …) and `attempt_forward` for
//! free-text chat — exactly as `runtime::predispatch::handle_slash_passthrough`
//! already does for the one-shot `trusty-agents /status`-style CLI
//! passthrough. Bypassing `ReplBridge` (the TUI's event/picker layer) means
//! `/model` and `/provider` with no argument print their list as plain text
//! instead of opening a ratatui modal — the whole point of this mode.
//! What: `run_plain_cli` — builds a `TrustyAgentsRepl`, optionally switches
//! it into thin-client mode when a `--service` daemon is already running
//! (mirrors the TUI startup path in `mode_dispatch`), prints a startup
//! banner showing the resolved agent model/runner/credential, then loops
//! reading one line at a time until `/exit`, `/quit`, `/disconnect`, or EOF
//! (Ctrl-D).
//! Test: `should_force_plain_cli_*` (mode-dispatch selector, in
//! `runtime::cli_def`); `tests/plain_cli_repl.rs` (piped-stdin integration
//! test driving the real compiled binary).

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::profile::load_or_create_user_profile;
use crate::ctrl::state::ConversationTurn;

/// Cap on retained conversation turns for the plain CLI loop.
///
/// Why: Mirrors `repl::dispatch::MAX_HISTORY_TURNS`, which is private to that
/// module — this is an intentional small duplicate of the constant (not the
/// dispatch logic) rather than widening that module's visibility for one use.
const PLAIN_CLI_MAX_HISTORY_TURNS: usize = 20;

/// Entry point for the persistent plain-line CLI mode. See module docs.
pub async fn run_plain_cli() -> Result<()> {
    let user_profile = load_or_create_user_profile().await?;
    let mut repl = crate::repl::TrustyAgentsRepl::new(user_profile)?;

    if crate::service::is_service_running(crate::service::DEFAULT_SERVICE_PORT).await {
        let url = format!("http://localhost:{}", crate::service::DEFAULT_SERVICE_PORT);
        let started = crate::service::read_pid_file()
            .map(|s| {
                format!(
                    "pid {} port {} since {}",
                    s.pid,
                    s.port,
                    s.started_at.to_rfc3339()
                )
            })
            .unwrap_or_else(|| format!("port {}", crate::service::DEFAULT_SERVICE_PORT));
        eprintln!("--- connected to running trusty-agents service ---");
        eprintln!("    {started}");
        eprintln!("    (use `/service stop` to shut it down)");
        repl.set_service_client_mode(url);
    }

    println!(
        "{} — plain CLI mode (no TUI)\ntype /help for commands, /quit or /exit to leave\n",
        crate::build_info::version_string()
    );
    println!(
        "resolved agent endpoint: model={} runner={:?} credential={}\n",
        repl.resolve_active_model(),
        repl.resolve_active_runner(),
        resolved_credential_label(),
    );

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    loop {
        stdout
            .write_all(b"you> ")
            .await
            .context("failed to write prompt")?;
        stdout.flush().await.context("failed to flush prompt")?;

        let mut line = String::new();
        let n = stdin
            .read_line(&mut line)
            .await
            .context("failed to read stdin")?;
        if n == 0 {
            println!("Bye.");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match repl.try_handle_slash(trimmed).await {
            Some(Ok((cont, output))) => {
                if !output.is_empty() {
                    print!("{output}");
                    if !output.ends_with('\n') {
                        println!();
                    }
                }
                if !cont {
                    break;
                }
            }
            Some(Err(e)) => {
                eprintln!("command error: {e:#}");
            }
            None => {
                dispatch_free_text(&mut repl, trimmed).await;
            }
        }
    }

    Ok(())
}

/// Forward one free-text (non-slash) line through `attempt_forward` — the
/// same LLM dispatch the ratatui TUI's submit path uses — print the
/// response, and append the turn to `conversation_history` so multi-turn
/// context keeps flowing exactly like the TUI.
async fn dispatch_free_text(repl: &mut crate::repl::TrustyAgentsRepl, trimmed: &str) {
    let start = std::time::Instant::now();
    match repl.attempt_forward(trimmed).await {
        Ok((response, _usage)) => {
            let elapsed = start.elapsed();
            println!("{response}");
            if repl.conversation_history.len() >= PLAIN_CLI_MAX_HISTORY_TURNS {
                repl.conversation_history.remove(0);
            }
            repl.conversation_history.push(ConversationTurn {
                user: trimmed.to_string(),
                assistant: response,
            });
            eprintln!("[TIMING] responded in {:.2}s", elapsed.as_secs_f64());
        }
        Err(e) => {
            let elapsed = start.elapsed();
            eprintln!("task error: {e:#}");
            eprintln!("[TIMING] failed after {:.2}s", elapsed.as_secs_f64());
        }
    }
}

/// Which credential env var `run_plain_cli`'s startup banner resolved, so an
/// SSH tester can see at a glance which auth path a message will use.
/// Mirrors the precedence documented in `cli_def::check_credentials_and_warn`.
fn resolved_credential_label() -> &'static str {
    if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").is_ok_and(|v| !v.is_empty()) {
        "CLAUDE_CODE_OAUTH_TOKEN"
    } else if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|v| !v.is_empty()) {
        "ANTHROPIC_API_KEY"
    } else if std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY)
        .is_ok_and(|v| !v.is_empty())
    {
        "OPENROUTER_API_KEY"
    } else {
        "none configured"
    }
}
