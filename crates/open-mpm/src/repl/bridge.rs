//! `ReplBridge` — the glue that wires the ratatui front-end's `ReplHandler`
//! trait to the `OpenMpmRepl` slash-command surface.
//!
//! Why: The ratatui driver in `tui.rs` is generic over `ReplHandler`. We
//! could not move slash dispatch into the driver itself without dragging
//! in all REPL state, so the bridge owns an `Arc<Mutex<OpenMpmRepl>>` and
//! adapts trait callbacks into slash-command + LLM-forward calls.
//! What: Implements `handle_input` — the single trait method. Intercepts
//! a handful of slash commands inline (`/exit`, `/clear`, `/model` no-arg,
//! `/provider` no-arg, `/switch` no-arg, `/update`) because they need
//! direct access to `tx` for picker overlays or progress streaming.
//! Everything else delegates to `repl.try_handle_slash` or the LLM dispatch.
//! Test: Exercised end-to-end by `scripts/tmux-repl-test.sh`; unit-tested
//! indirectly via `try_handle_slash_*` in `mod.rs::tests`.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use super::OpenMpmRepl;
use super::agent_commands::detect_agent_switch;
use super::tui;

/// Bridge implementing the ratatui `ReplHandler` trait against an
/// `Arc<Mutex<OpenMpmRepl>>`.
pub(crate) struct ReplBridge {
    pub(crate) repl: Arc<tokio::sync::Mutex<OpenMpmRepl>>,
}

#[async_trait::async_trait]
impl tui::ReplHandler for ReplBridge {
    async fn handle_input(
        &self,
        line: String,
        tx: tokio::sync::mpsc::UnboundedSender<tui::ReplEvent>,
    ) -> Result<bool> {
        let trimmed = line.trim();

        // Persist to on-disk history before dispatch.
        {
            let repl = self.repl.lock().await;
            append_history_line(&repl.history_path, trimmed);
        }

        // Natural-language agent switching for short phrases.
        let word_count = trimmed.split_whitespace().count();
        if word_count < 10 {
            let mut repl = self.repl.lock().await;
            if let Some(target) = detect_agent_switch(trimmed, repl.active_persona.is_some()) {
                if target == "ctrl" {
                    repl.active_persona = None;
                    repl.project_name = "ctrl".to_string();
                    repl.conversation_history.clear();
                    repl.chat_log.clear();
                    let _ = tx.send(tui::ReplEvent::LabelChanged("ctrl".into()));
                    let _ = tx.send(tui::ReplEvent::AgentScopeChanged(tui::AgentScope::User));
                    let _ = tx.send(tui::ReplEvent::StatusMessage("Switching to: ctrl".into()));
                } else {
                    let _ = tx.send(tui::ReplEvent::StatusMessage(format!(
                        "Switching to: {}",
                        target
                    )));
                    repl.handle_agent_command(target);
                    let _ = tx.send(tui::ReplEvent::LabelChanged(repl.project_name.clone()));
                    // Persona agents are always user-scoped.
                    let _ = tx.send(tui::ReplEvent::AgentScopeChanged(tui::AgentScope::User));
                }
                return Ok(true);
            }
        }

        // Try slash command first.
        if trimmed.starts_with('/') {
            let cmd = trimmed.split(char::is_whitespace).next().unwrap_or("");
            if cmd == "/exit" || cmd == "/quit" || cmd == "/disconnect" {
                return Ok(false);
            }
            if cmd == "/clear" {
                let mut repl = self.repl.lock().await;
                repl.conversation_history.clear();
                repl.chat_log.clear();
                repl.active_persona = None;
                repl.project_name = "ctrl".to_string();
                // #284: Reset session overrides so /clear is a true session reset.
                repl.model_override = None;
                repl.provider_override = None;
                let _ = tx.send(tui::ReplEvent::LabelChanged("ctrl".into()));
                let _ = tx.send(tui::ReplEvent::AgentScopeChanged(tui::AgentScope::User));
                let _ = tx.send(tui::ReplEvent::TokenReset);
                let _ = tx.send(tui::ReplEvent::StatusMessage(
                    "Conversation history cleared.".into(),
                ));
                return Ok(true);
            }
            // Intercept `/model` and `/provider` with no arg → open the
            // interactive picker overlay instead of printing the current
            // value as text. The picker emits a synthetic `Submit` for
            // `/model <selected>` / `/provider <selected>` on Enter, so the
            // existing slash handler still does the actual mutation.
            //
            // Why: Typing model ids by hand is error-prone (which Anthropic
            // alias is current?). A list overlay shows the valid set and
            // requires zero typing.
            // What: For `/provider` with no arg, items are the static valid
            // set. For `/model` with no arg, items are the cached ollama
            // models when `provider_override == "local"`, otherwise a
            // hardcoded short list of common Anthropic models.
            // Test: Manual via tmux REPL.
            let cmd_only = trimmed
                .split(char::is_whitespace)
                .next()
                .unwrap_or("")
                .trim();
            let arg_only = trimmed
                .split_once(char::is_whitespace)
                .map(|x| x.1)
                .map(str::trim)
                .unwrap_or("");
            if arg_only.is_empty() && cmd_only == "/switch" {
                // Inline persona list (NOT a modal overlay). Items are the
                // friendly display names; the `/switch` slash handler
                // maps them back to TOML stems. The `"switch"` context
                // tag tells the inline-choice Enter handler to directly
                // dispatch `/switch <selected>` instead of inserting the
                // selected text into the input buffer for a second Enter.
                let items: Vec<String> = vec![
                    "ctrl".to_string(),
                    "Izzie".to_string(),
                    "CTO Assistant".to_string(),
                ];
                let _ = tx.send(tui::ReplEvent::SetChoices {
                    items,
                    context: Some("switch".to_string()),
                });
                return Ok(true);
            }
            if arg_only.is_empty() && (cmd_only == "/model" || cmd_only == "/provider") {
                let repl = self.repl.lock().await;
                if cmd_only == "/model" {
                    let items: Vec<String> = if repl.provider_override.as_deref() == Some("local")
                        && !repl.ollama_models.is_empty()
                    {
                        repl.ollama_models.clone()
                    } else {
                        vec![
                            "anthropic/claude-haiku-4-5".to_string(),
                            "anthropic/claude-sonnet-4-6".to_string(),
                            "anthropic/claude-opus-4-6".to_string(),
                        ]
                    };
                    let _ = tx.send(tui::ReplEvent::OpenPicker {
                        items,
                        title: "Select Model".to_string(),
                        kind: tui::PickerKind::Model,
                    });
                } else {
                    // #293: openrouter is now the documented default — list it
                    // first in the provider picker so the highlighted entry on
                    // open is the default route rather than claude-code.
                    let items: Vec<String> = ["openrouter", "claude-code", "bedrock", "local"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    let _ = tx.send(tui::ReplEvent::OpenPicker {
                        items,
                        title: "Select Provider".to_string(),
                        kind: tui::PickerKind::Provider,
                    });
                }
                return Ok(true);
            }

            // #368: `/update` is intercepted here (rather than in
            // `try_handle_slash`) because it streams progress via `tx` and
            // shells out to `cargo install`, which the slash handler's
            // `(bool, String)` return shape can't express.
            if cmd_only == "/update" {
                match crate::update::check_for_update().await {
                    Some(info) => {
                        let msg = format!(
                            "Upgrading from v{} to v{}…\nRunning: cargo install --git https://github.com/bobmatnyc/open-mpm open-mpm",
                            env!("CARGO_PKG_VERSION"),
                            info.latest_version
                        );
                        let _ = tx.send(tui::ReplEvent::StatusMessage(msg));
                        match tokio::process::Command::new("cargo")
                            .args([
                                "install",
                                "--git",
                                "https://github.com/bobmatnyc/open-mpm",
                                "open-mpm",
                            ])
                            .status()
                            .await
                        {
                            Ok(status) if status.success() => {
                                let done = format!(
                                    "✓ Upgraded to v{}. Restart open-mpm to use the new version.",
                                    info.latest_version
                                );
                                let _ = tx.send(tui::ReplEvent::StatusMessage(done));
                            }
                            Ok(status) => {
                                let _ = tx.send(tui::ReplEvent::StatusMessage(format!(
                                    "✗ Upgrade failed (exit {}). Try: cargo install --git https://github.com/bobmatnyc/open-mpm open-mpm",
                                    status
                                )));
                            }
                            Err(e) => {
                                let _ = tx.send(tui::ReplEvent::StatusMessage(format!(
                                    "✗ Failed to run cargo install: {e}"
                                )));
                            }
                        }
                    }
                    None => {
                        let _ = tx.send(tui::ReplEvent::StatusMessage(format!(
                            "open-mpm v{} is up to date.",
                            env!("CARGO_PKG_VERSION")
                        )));
                    }
                }
                return Ok(true);
            }

            let mut repl = self.repl.lock().await;
            match repl.try_handle_slash(trimmed).await {
                Some(Ok((cont, output))) => {
                    let trimmed_out = output.trim_end_matches('\n').to_string();
                    if !trimmed_out.is_empty() {
                        let _ = tx.send(tui::ReplEvent::LlmResponse {
                            text: trimmed_out,
                            is_error: false,
                        });
                    }
                    if !cont {
                        return Ok(false);
                    }
                    let _ = tx.send(tui::ReplEvent::LabelChanged(repl.project_name.clone()));
                    // Emit scope after any slash command that may have changed the active
                    // agent (e.g. /connect sets Project scope, /agent sets User scope).
                    let _ = tx.send(tui::ReplEvent::AgentScopeChanged(repl.current_scope()));
                    // Refresh statusline so /model and /provider edits are picked up
                    // immediately. Effective model = override → otherwise the agent
                    // TOML's resolved model. Effective provider = override → otherwise
                    // the env-derived credential label.
                    let model_eff = repl
                        .model_override
                        .clone()
                        .unwrap_or_else(|| repl.resolve_active_model());
                    let provider_eff = repl.provider_override.clone().unwrap_or_else(|| {
                        crate::llm::credentials::pick_credentials(Some(
                            repl.resolve_active_runner(),
                        ))
                        .map(|c| c.label().to_string())
                        .unwrap_or_else(|| "none".to_string())
                    });
                    let _ = tx.send(tui::ReplEvent::StatuslineUpdate {
                        model: model_eff,
                        provider: provider_eff,
                    });
                    // After any slash command, sync the cached ollama model
                    // list to the TUI so the next `/model` picker shows
                    // current data. Cheap clone (small Vec<String>).
                    let _ = tx.send(tui::ReplEvent::OllamaModelsLoaded(
                        repl.ollama_models.clone(),
                    ));
                    return Ok(true);
                }
                Some(Err(e)) => {
                    let _ = tx.send(tui::ReplEvent::LlmResponse {
                        text: format!("error: {e:#}"),
                        is_error: true,
                    });
                    return Ok(true);
                }
                None => {}
            }
        }

        // Forward to LLM.
        let mut repl = self.repl.lock().await;
        repl.forward_task_to_channel(&line, tx.clone()).await?;
        let _ = tx.send(tui::ReplEvent::LabelChanged(repl.project_name.clone()));
        Ok(true)
    }
}

/// Append a single submitted line to the on-disk history file.
pub(crate) fn append_history_line(path: &Path, line: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}
