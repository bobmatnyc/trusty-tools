//! LLM dispatch + thinking-relay helpers for the REPL.
//!
//! Why: `forward_task_to_channel` and `attempt_forward` glue the REPL to
//! the controller socket / persona runner / service-client paths; pulling
//! them out of `mod.rs` keeps the orchestration logic in one place and
//! shrinks the top-level module.
//! What: `TrustyAgentsRepl::forward_task_to_channel` streams progress via
//! `ReplEvent::ThinkingStep` while the LLM runs; `attempt_forward` decides
//! whether to call the service-client, the persona runner, the controller
//! socket, or fall back to in-process PM dispatch; `spawn_thinking_relay`
//! subscribes to the global event bus and translates events to thinking
//! steps until the LLM call returns.
//! Test: Covered transitively by `try_handle_slash_*` and the tmux REPL
//! end-to-end script.

use std::sync::Arc;

use anyhow::Result;

use crate::ctrl::{self, CtrlSocket};
use crate::perf::TokenUsage;

use super::TrustyAgentsRepl;
use super::tui;

/// Cap on retained conversation turns (mirrors the constant in mod.rs).
const MAX_HISTORY_TURNS: usize = 20;

impl TrustyAgentsRepl {
    /// Forward `task_text` and stream the result through `tx`. Emits
    /// `ReplEvent`s instead of printing — the tui renderer owns the actual
    /// display. Also subscribes to the global `events::Event` bus and
    /// translates relevant signals into `ThinkingStep` updates so the user
    /// sees progress (delegating to engineer, generating code, etc.).
    pub(crate) async fn forward_task_to_channel(
        &mut self,
        task_text: &str,
        tx: tokio::sync::mpsc::UnboundedSender<tui::ReplEvent>,
    ) -> Result<()> {
        let _ = tx.send(tui::ReplEvent::LlmThinking(true));
        // Initial generic "thinking" line so the user sees feedback within a
        // render tick of pressing Enter, even before the first event lands.
        let _ = tx.send(tui::ReplEvent::ThinkingStep("thinking…".to_string()));

        // Feature B2: Log the user message before dispatch so the on-disk
        // record orders strictly user → assistant per turn.
        let agent_label = self
            .active_persona
            .clone()
            .unwrap_or_else(|| self.project_name.clone());
        crate::logging::log(crate::logging::LogEntry::Message {
            ts: chrono::Utc::now(),
            role: "user".to_string(),
            content: task_text.to_string(),
            agent: agent_label.clone(),
            tokens: None,
        });

        // Spawn a background relay that translates global events into
        // ThinkingStep messages until the task completes. The cancel flag is
        // set after the LLM call returns so the relay shuts down cleanly.
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        spawn_thinking_relay(tx.clone(), cancel.clone());

        let result = self.attempt_forward(task_text).await;
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);

        match result {
            Ok((response, _usage)) => {
                // Token tracking now flows through the event bus →
                // `ReplEvent::TokenUpdate` → ratatui input bar. The legacy
                // `status_bar.add_tokens` path was a no-op (attempt_forward
                // always returned zeros) and wrote to stderr, which the TUI
                // doesn't render — removed to avoid confusion.
                self.status_bar.set_agent(None);
                if self.conversation_history.len() >= MAX_HISTORY_TURNS {
                    self.conversation_history.remove(0);
                }
                self.conversation_history
                    .push(crate::ctrl::ConversationTurn {
                        user: task_text.to_string(),
                        assistant: response.clone(),
                    });
                // Feature B2: Persist the assistant turn alongside the user
                // turn logged above. `_usage` is currently a placeholder
                // (zeros) on the REPL path; thread it through once the LLM
                // dispatch surfaces real token counts.
                crate::logging::log(crate::logging::LogEntry::Message {
                    ts: chrono::Utc::now(),
                    role: "assistant".to_string(),
                    content: response.clone(),
                    agent: agent_label.clone(),
                    tokens: None,
                });
                let _ = tx.send(tui::ReplEvent::LlmResponse {
                    text: response,
                    is_error: false,
                });
                Ok(())
            }
            Err(e) => {
                let _ = tx.send(tui::ReplEvent::LlmResponse {
                    text: format!("error: {e:#}"),
                    is_error: true,
                });
                Ok(())
            }
        }
    }

    /// Probe the controller socket; if unavailable, run the PM task in-process.
    ///
    /// #271: When no persona is active and no controller socket is alive, the
    /// dispatch target depends on `AgentScope`:
    /// - `AgentScope::User` (direct ctrl chat) → call ctrl directly via
    ///   `run_pm_task_with_persona("ctrl", …)`. This avoids spinning up the
    ///   PM orchestrator (and its delegation toolset / "PM thinking…" event
    ///   stream) for what is just a user-level conversation.
    /// - `AgentScope::Project` → keep the existing PM orchestrator path
    ///   (`run_pm_task_with_history`) so project tasks can still delegate
    ///   to sub-agents.
    pub(crate) async fn attempt_forward(&self, task_text: &str) -> Result<(String, TokenUsage)> {
        // #343: Thin-client mode — forward to the running daemon over HTTP
        // and bypass all in-process dispatch (persona, ctrl socket, PM).
        // Token usage isn't reported by the HTTP API today, so we return
        // a default `TokenUsage` until the API surfaces it.
        if let Some(server) = self.service_url.as_deref() {
            let narrative = crate::service::submit_task_via_service(server, task_text).await?;
            return Ok((narrative, TokenUsage::default()));
        }

        // #284: Build the session overrides snapshot once so every dispatch
        // arm in this function uses the same `/model` and `/provider` state.
        let overrides = crate::ctrl::SessionOverrides {
            model: self.model_override.clone(),
            provider: self.provider_override.clone(),
            // #481: REPL dispatch runs as the trusted local CLI operator
            // (`UserIdentity::default()` → `All` tier). Slack RBAC identities
            // are threaded only through the Slack transport.
            user: None,
        };
        if let Some(persona_name) = self.active_persona.as_deref() {
            let response = crate::ctrl::run_pm_task_with_persona(
                &self.project_dir,
                persona_name,
                task_text,
                &self.conversation_history,
                None,
                overrides,
            )
            .await?;
            return Ok((response, TokenUsage::default()));
        }
        match CtrlSocket::probe_default(&self.socket_path).await {
            Ok(stream) => {
                // #271/#283: AgentScope::User (direct ctrl chat) must bypass
                // the controller socket — even when it's alive. Forwarding
                // user-scope messages through the socket lands them in the PM
                // orchestrator (`run_pm_task_with_history`), which emits
                // `PmThinking` events and pulls the full delegation toolset.
                // For ctrl direct chat we want the lightweight persona path
                // (`run_pm_task_with_persona("ctrl", …)`) regardless of socket
                // state. Project scope keeps the existing socket-forward
                // behavior so PM orchestration / delegation still works.
                match self.current_scope() {
                    tui::AgentScope::User => {
                        // Drop the probed stream so we don't leak the
                        // controller connection; ctrl persona runs in-process.
                        drop(stream);
                        let response = crate::ctrl::run_pm_task_with_persona(
                            &self.project_dir,
                            "ctrl",
                            task_text,
                            &self.conversation_history,
                            None,
                            overrides,
                        )
                        .await?;
                        Ok((response, TokenUsage::default()))
                    }
                    tui::AgentScope::Project => {
                        let response = ctrl::forward_to_controller(
                            stream,
                            task_text.to_string(),
                            &self.conversation_history,
                            &self.project_dir,
                        )
                        .await?;
                        Ok((response, TokenUsage::default()))
                    }
                }
            }
            Err(_) => {
                // #271: User scope (direct ctrl chat) skips the PM orchestrator
                // and goes straight to the ctrl persona path. This eliminates
                // the spurious `⟳ PM thinking…` events seen when chatting with
                // the user-level ctrl agent — there is no PM to orchestrate.
                let response = match self.current_scope() {
                    tui::AgentScope::User => {
                        crate::ctrl::run_pm_task_with_persona(
                            &self.project_dir,
                            "ctrl",
                            task_text,
                            &self.conversation_history,
                            None,
                            overrides,
                        )
                        .await?
                    }
                    tui::AgentScope::Project => {
                        crate::ctrl::run_pm_task_with_history(
                            &self.project_dir,
                            task_text,
                            &self.conversation_history,
                            None,
                            overrides,
                        )
                        .await?
                    }
                };
                Ok((response, TokenUsage::default()))
            }
        }
    }
}

/// Spawn a background task that subscribes to the global event bus and
/// translates relevant events into `ThinkingStep` messages on `tx` until
/// `cancel` is set.
///
/// Why: While the LLM is working, the user benefits from seeing curated
/// status lines (delegating to engineer, generating code, etc.) rather than
/// staring at an opaque `[thinking...]`. This relay watches the global
/// `events::Event` bus — which is already populated by the PM/agent
/// lifecycle — and emits human-readable updates to the tui.
/// What: Runs until `cancel` flips. Each tick re-checks `cancel` so the
/// relay shuts down promptly when the LLM call returns.
/// Test: Manual via tmux REPL test.
pub(crate) fn spawn_thinking_relay(
    tx: tokio::sync::mpsc::UnboundedSender<tui::ReplEvent>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) {
    use crate::events::{self, Event};
    use std::sync::atomic::Ordering;
    tokio::spawn(async move {
        let mut rx = events::subscribe();
        loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(ev)) => {
                    // Relay token usage to the live counters in the input bar.
                    // `LlmRequested` carries `prompt_tokens` (when known), and
                    // `LlmResponded` carries `completion_tokens`. We send each
                    // as a partial update — accumulation happens in the TUI.
                    match &ev {
                        Event::LlmRequested { prompt_tokens, .. } => {
                            if let Some(p) = prompt_tokens
                                && tx
                                    .send(tui::ReplEvent::TokenUpdate {
                                        prompt: *p as u64,
                                        completion: 0,
                                    })
                                    .is_err()
                            {
                                break;
                            }
                        }
                        Event::LlmResponded {
                            completion_tokens, ..
                        } => {
                            if let Some(c) = completion_tokens
                                && tx
                                    .send(tui::ReplEvent::TokenUpdate {
                                        prompt: 0,
                                        completion: *c as u64,
                                    })
                                    .is_err()
                            {
                                break;
                            }
                        }
                        _ => {}
                    }

                    let step: Option<String> = match &ev {
                        Event::PmDelegating { agent, .. } => {
                            Some(format!("Delegating to {agent}…"))
                        }
                        Event::AgentSpawned { agent, .. } => Some(format!("{agent} · spawning…")),
                        Event::AgentStarted {
                            agent_name,
                            runner_type,
                            ..
                        } => Some(format!("{agent_name} · running ({runner_type})…")),
                        Event::PmThinking { .. } => Some("PM thinking…".to_string()),
                        Event::ToolCalled { tool, .. } => Some(format!("Calling tool: {tool}…")),
                        Event::AgentDone { agent, .. } => Some(format!("{agent} · done")),
                        Event::AgentFailed { agent, .. } => Some(format!("{agent} · failed")),
                        Event::AstOperation { op, detail, .. } => {
                            Some(format!("AST {op}: {detail}"))
                        }
                        _ => None,
                    };
                    if let Some(s) = step
                        && tx.send(tui::ReplEvent::ThinkingStep(s)).is_err()
                    {
                        break;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
    });
}
