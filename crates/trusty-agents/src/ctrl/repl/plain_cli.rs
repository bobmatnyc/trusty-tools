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
//! What: `run_plain_cli` — builds a `TrustyAgentsRepl`, defaults its active
//! conversational agent to `assistant` (Sonnet, #3688) by internally issuing
//! `/switch assistant` — the same mechanism a user typing that command would
//! trigger — so the owner's actual testing surface is answered by the
//! assistant persona out of the box, not the `ctrl` coordinator (local
//! ollama). `ctrl` remains fully reachable via `/switch ctrl`, and
//! machine-coordination commands (`/connect`, `/status`, controller socket)
//! are untouched. Optionally switches into thin-client mode when a
//! `--service` daemon is already running (mirrors the TUI startup path in
//! `mode_dispatch`), prints a startup banner showing the resolved agent
//! name/model/runner/credential, then loops reading one line at a time
//! until `/exit`, `/quit`, `/disconnect`, or EOF (Ctrl-D).
//! Test: `should_force_plain_cli_*` (mode-dispatch selector, in
//! `runtime::cli_def`); `tests/plain_cli_repl.rs` (piped-stdin integration
//! test driving the real compiled binary).

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::profile::load_or_create_user_profile;
use crate::agents::RunnerKind;
use crate::ctrl::state::ConversationTurn;

/// Persona the plain CLI defaults its conversational agent to at startup
/// (owner decision, #3406 follow-up; reaffirmed by #3738): the generic
/// "Assistant" is the default starting agent an SSH tester exercises, NOT the
/// deprecated `ctrl` machine-coordination persona (local ollama). `ctrl`
/// stays one `/switch ctrl` away but nothing defaults to it.
const DEFAULT_PERSONA: &str = "assistant";

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

    // Owner decision (#3406 follow-up): default the plain CLI's active
    // conversational agent to `assistant` instead of the `ctrl` coordinator
    // it inherits from `TrustyAgentsRepl::new`. Reuses the SAME dispatch a
    // user typing `/switch assistant` would trigger — not a new mechanism —
    // so behavior (persona resolution, display-name lookup, history reset)
    // matches exactly. Missing/broken `assistant/agent.toml` degrades
    // gracefully: the error is surfaced and the session stays on `ctrl`.
    match repl
        .try_handle_slash(&format!("/switch {DEFAULT_PERSONA}"))
        .await
    {
        Some(Ok((_, output))) => {
            if !output.is_empty() {
                print!("{output}");
                if !output.ends_with('\n') {
                    println!();
                }
            }
        }
        Some(Err(e)) => {
            eprintln!(
                "warning: failed to default to the '{DEFAULT_PERSONA}' persona ({e:#}); staying on ctrl"
            );
        }
        None => unreachable!("\"/switch ...\" always starts with '/'"),
    }

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
    let (agent_name, model, runner) = resolve_endpoint_agent(&repl);
    println!(
        "resolved agent endpoint: agent={} model={} runner={:?} credential={}\n",
        agent_name,
        model,
        runner,
        resolved_credential_label(runner),
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

/// Resolve the agent name/model/runner the startup banner should display,
/// reflecting whichever persona is ACTUALLY active after the default-persona
/// switch above (or a later `/switch`/`/agent`).
///
/// Why: `TrustyAgentsRepl::resolve_active_model` / `resolve_active_runner`
/// only ever consult `ctrl.toml` / `pm.toml` (see their doc comments in
/// `repl/mod.rs`) — they predate per-persona chat and don't know about
/// `active_persona`. Using them directly here would keep showing `ctrl`'s
/// model/runner even after defaulting (or switching) to `assistant`. This
/// resolves the SAME way `/agent`/`/switch` do — `AgentConfig::by_name_in`
/// against the REPL's own `agents_dir` + the `$HOME` fallback — so the
/// banner always matches what the next `attempt_forward` call will actually
/// dispatch. Falls back to the ctrl-only resolvers if the lookup fails
/// (e.g. a persona whose TOML was deleted after activation).
/// What: Returns `(agent_name, model_id, runner_kind)`.
fn resolve_endpoint_agent(repl: &crate::repl::TrustyAgentsRepl) -> (String, String, RunnerKind) {
    let agent_name = repl
        .active_persona
        .clone()
        .unwrap_or_else(|| "ctrl".to_string());

    let mut dirs = vec![repl.agents_dir.clone()];
    if let Some(home) = dirs::home_dir() {
        let home_dir = home.join(".trusty-agents").join("agents");
        if home_dir != repl.agents_dir {
            dirs.push(home_dir);
        }
    }

    match crate::agents::AgentConfig::by_name_in(&dirs, &agent_name) {
        Ok(cfg) => (agent_name, cfg.agent.model.clone(), cfg.agent.runner),
        Err(_) => (
            agent_name,
            repl.resolve_active_model(),
            repl.resolve_active_runner(),
        ),
    }
}

/// Which credential source `run_plain_cli`'s startup banner resolved, so an
/// SSH tester can see at a glance which auth path a message will use.
///
/// Why (#3407): the previous implementation read `std::env::var(...)`
/// directly, so a credential configured ONLY via the secure keystore
/// (`tagent config keys set`, never exported to the shell) showed as "none
/// configured" even though dispatch would resolve and use it fine. Routes
/// through the same `pick_credentials` resolver the TUI's own startup
/// banner uses (`repl/run.rs`), which checks env → `.env.local` → the store.
///
/// Issue #3406 follow-up: a bare `credential=none` was confusing when the
/// secure store demonstrably HAD a key — just for a provider (e.g.
/// `fireworks`) ctrl/PM's own LLM calls don't route through. When
/// `pick_credentials` finds nothing but
/// [`crate::llm::credentials::other_configured_providers`] names one, the
/// label now says so explicitly instead of a bare "none".
fn resolved_credential_label(runner: RunnerKind) -> String {
    if let Some(c) = crate::llm::credentials::pick_credentials(Some(runner)) {
        return c.label().to_string();
    }
    let other = crate::llm::credentials::other_configured_providers();
    if other.is_empty() {
        "none".to_string()
    } else {
        format!(
            "none (configured: {} — not usable for this agent's model; need one of \
             openrouter/anthropic/claude-code)",
            other.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_PERSONA;

    /// #3738/#3406: the plain CLI must default to the generic "assistant"
    /// persona at startup, never the deprecated `ctrl` coordinator.
    #[test]
    fn default_persona_is_generic_assistant_not_ctrl() {
        assert_eq!(DEFAULT_PERSONA, "assistant");
        assert_ne!(DEFAULT_PERSONA, "ctrl");
    }
}
