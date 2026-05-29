//! Part of the `commands` module (split from the monolithic `commands.rs`
//! for the 500-line file cap — see #357). Holds an `impl OpenMpmRepl` block
//! for one slash-command handler group.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::Result;

use super::write_help;
use crate::repl::OpenMpmRepl;

impl OpenMpmRepl {
    /// Handle slash commands. Returns `None` when `input` is not a slash
    /// command so the caller can fall through to task forwarding.
    /// Dispatch a slash command and capture all output as a `String`.
    ///
    /// Why: In ratatui mode, direct stdout/stderr writes corrupt the alt-screen
    ///   buffer. All output must flow through `ReplEvent` so the renderer owns
    ///   the display. This method collects the textual result of a slash
    ///   command into a buffer that the caller can forward as a `LlmResponse`
    ///   event (or print directly in non-tui paths).
    /// What: Returns `None` for non-slash input, otherwise
    ///   `Some(Ok((continue_running, output)))` where `continue_running == false`
    ///   signals exit and `output` is the captured display text. Errors flow
    ///   through `Some(Err(_))` for the caller to surface.
    /// Test: `try_handle_slash_*` unit tests verify return shape; tmux e2e
    ///   verifies `/help` renders cleanly inside ratatui.
    // Why: Widened from `pub(crate)` to `pub` so the `open-mpm` binary
    //      (now a separate crate consuming `open-mpm` as a library) can
    //      invoke `repl.try_handle_slash(...)` from `main.rs`.
    // What: Public async method; signature otherwise unchanged.
    // Test: Existing slash-command unit tests + tmux e2e cover behaviour.
    pub async fn try_handle_slash(&mut self, input: &str) -> Option<Result<(bool, String)>> {
        if !input.starts_with('/') {
            return None;
        }
        let mut parts = input.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().map(str::trim).unwrap_or("");

        let mut out = String::new();
        let result: Result<bool> = match cmd {
            "/help" => {
                write_help(&mut out);
                Ok(true)
            }
            "/exit" | "/quit" | "/disconnect" => {
                // #404: When the REPL is operating as a thin client against a
                // remote `open-mpm` daemon, exiting the client should NOT take
                // the server down with it. Surface this clearly so the user
                // knows the server is still running and how to stop it.
                if let Some(server) = self.service_url.as_deref() {
                    let port_hint = server
                        .rsplit_once(':')
                        .map(|(_, p)| p.trim_end_matches('/').to_string())
                        .unwrap_or_else(|| "8765".to_string());
                    let _ = writeln!(
                        out,
                        "Disconnected from server. Server still running on port {port_hint}. Use `om stop` to shut it down."
                    );
                } else {
                    let _ = writeln!(out, "Bye.");
                }
                Ok(false)
            }
            "/clear" => {
                self.conversation_history.clear();
                self.chat_log.clear();
                self.active_persona = None;
                self.project_name = "ctrl".to_string();
                // #284: Session overrides are per-session — clear them too.
                self.model_override = None;
                self.provider_override = None;
                let _ = writeln!(out, "Conversation history cleared.");
                Ok(true)
            }
            "/provider" => {
                if arg == "local" {
                    self.handle_provider_local_into(&mut out).await;
                } else {
                    self.handle_provider_command_into(arg, &mut out);
                }
                Ok(true)
            }
            "/model" => {
                self.handle_model_command_into(arg, &mut out);
                Ok(true)
            }
            "/agent" => {
                self.handle_agent_command_into(arg, &mut out);
                Ok(true)
            }
            "/switch" => {
                // Why: `/switch` is the user-facing way to flip the active
                // front-end "voice" between the three blessed personas
                // (ctrl, Izzie, CTO Assistant) without remembering the
                // underlying TOML stems. Direct args go straight through;
                // empty arg is intercepted upstream by `ReplBridge` to open
                // the picker — by the time we get here with empty arg the
                // user typed `/switch` literally, so list the choices.
                self.handle_switch_command_into(arg, &mut out);
                Ok(true)
            }
            "/agents" => {
                self.print_agents_into(&mut out);
                Ok(true)
            }
            "/skills" => {
                self.print_skills_into(&mut out);
                Ok(true)
            }
            "/memories" => {
                self.run_memories_into(arg, &mut out).await;
                Ok(true)
            }
            "/status" => {
                if let Err(e) = self.send_status_command_into(&mut out).await {
                    let _ = writeln!(out, "status error: {e:#}");
                }
                Ok(true)
            }
            "/session" => {
                let _ = writeln!(out, "Project: {}", self.project_name);
                let _ = writeln!(out, "Socket:  {}", self.socket_path.display());
                Ok(true)
            }
            "/connect" => {
                self.do_connect_tm_into(arg, &mut out).await;
                Ok(true)
            }
            "/cd" => {
                // #451: `/cd` keeps the legacy "switch REPL project context"
                // behavior; `/connect` is reserved for the TM session model
                // (`/connect <path> <adapter> [name]`). They diverged because
                // creating a tmux session and re-rooting the REPL are now
                // distinct operations.
                self.do_connect_into(arg, &mut out);
                Ok(true)
            }
            "/version" => {
                match crate::build_info::BuildInfo::load_and_increment().await {
                    Ok(info) => {
                        let _ = writeln!(out, "{}", info.display_string());
                    }
                    Err(_) => {
                        let _ = writeln!(
                            out,
                            "open-mpm v{} (build info unavailable)",
                            env!("CARGO_PKG_VERSION")
                        );
                    }
                }
                Ok(true)
            }
            "/projects" => {
                let show_all = arg.split_whitespace().any(|t| t == "--all");
                self.print_projects_into(&mut out, show_all).await;
                Ok(true)
            }
            "/log" => {
                let n: usize = if arg.is_empty() {
                    20
                } else {
                    arg.parse().unwrap_or(20)
                };
                self.tail_log_into(n, &mut out);
                Ok(true)
            }
            "/run" => {
                if arg.is_empty() {
                    let _ = writeln!(out, "usage: /run <file>");
                } else {
                    let path = PathBuf::from(arg);
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            let task = content.trim().to_string();
                            if task.is_empty() {
                                let _ = writeln!(out, "error: task file is empty");
                            } else {
                                let _ = writeln!(out, "→ Running task from {}", path.display());
                                match self.attempt_forward(&task).await {
                                    Ok((response, _)) => {
                                        let _ = writeln!(out, "{response}");
                                    }
                                    Err(e) => {
                                        let _ = writeln!(out, "error: {e:#}");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = writeln!(out, "error reading {}: {e}", path.display());
                        }
                    }
                }
                Ok(true)
            }
            "/history" => {
                let n: usize = if arg.is_empty() {
                    10
                } else {
                    arg.parse().unwrap_or(10)
                };
                self.print_history_into(n, &mut out);
                Ok(true)
            }
            "/telegram" => {
                self.handle_telegram_command_into(arg, &mut out).await;
                Ok(true)
            }
            "/slack" => {
                self.handle_slack_command_into(arg, &mut out).await;
                Ok(true)
            }
            "/logs" => {
                // Feature B5: Tail the last 20 entries from today's chat log
                // and pretty-print them as `[HH:MM] <role>: <preview>`. When
                // logging is disabled or the file doesn't exist yet, surface a
                // friendly hint rather than an error.
                self.print_recent_logs_into(&mut out);
                Ok(true)
            }
            "/local" => {
                // #319: local inference status / control. See
                // `handle_local_command_into` for argument semantics.
                self.handle_local_command_into(arg, &mut out).await;
                Ok(true)
            }
            "/tm" => {
                // Issue #316/#319: TM is always-on; route /tm subcommands
                // through the dispatcher. Tmux-missing errors surface from
                // the underlying orchestrator at command time.
                if let Err(e) = crate::tm::handle_tm_command(&self.tm_manager, arg, &mut out).await
                {
                    let _ = writeln!(out, "tm error: {e:#}");
                }
                Ok(true)
            }
            "/service" => {
                self.handle_service_command_into(arg, &mut out).await;
                Ok(true)
            }
            "/config" => {
                self.handle_config_command_into(arg, &mut out);
                Ok(true)
            }
            other => {
                let _ = writeln!(out, "unknown command: {other} (type /help)");
                Ok(true)
            }
        };

        Some(result.map(|cont| (cont, out)))
    }

    /// Handle `/service` subcommands (start/stop/status). Factored out of
    /// the dispatch table so `try_handle_slash` stays readable.
    async fn handle_service_command_into(&self, arg: &str, out: &mut String) {
        // #343: persistent daemon controls. `start` daemonizes
        // `open-mpm --serve` in the background; `stop` SIGTERMs
        // it; `status` (default) shows running state.
        let port = crate::service::DEFAULT_SERVICE_PORT;
        match arg.trim() {
            "start" => match crate::service::start_service(port).await {
                Ok(state) => {
                    let _ = writeln!(
                        out,
                        "service started: pid {} port {} (started {})",
                        state.pid,
                        state.port,
                        state.started_at.to_rfc3339()
                    );
                }
                Err(e) => {
                    let _ = writeln!(out, "service start failed: {e:#}");
                }
            },
            "stop" => match crate::service::stop_service().await {
                Ok(()) => {
                    let _ = writeln!(out, "service stopped");
                }
                Err(e) => {
                    let _ = writeln!(out, "service stop failed: {e:#}");
                }
            },
            "status" | "" => {
                let _ = writeln!(out, "{}", crate::service::status_line(port).await);
            }
            other => {
                let _ = writeln!(
                    out,
                    "unknown /service subcommand: {other} (use start | stop | status)"
                );
            }
        }
    }

    /// Handle `/config` subcommands. Factored out of the dispatch table.
    ///
    /// #371: Recap config inspection / control. Today this is a stub that
    /// surfaces the keys / defaults; the full RecapConfig lives in the API
    /// server's `recap_tracker` and persistence is a follow-up.
    fn handle_config_command_into(&self, arg: &str, out: &mut String) {
        let trimmed = arg.trim();
        if trimmed.is_empty() || trimmed == "recap" {
            let defaults = crate::recap::RecapConfig::default();
            let _ = writeln!(
                out,
                "recap config (defaults; live values are held by the API server):"
            );
            let _ = writeln!(out, "  recap.enabled  = {}", defaults.enabled);
            let _ = writeln!(out, "  recap.interval = {}", defaults.interval);
            let _ = writeln!(out, "Set with: /config recap.enabled <true|false>");
            let _ = writeln!(out, "          /config recap.interval <n>");
        } else if let Some(rest) = trimmed.strip_prefix("recap.enabled") {
            let val = rest.trim();
            match val.parse::<bool>() {
                Ok(b) => {
                    let _ = writeln!(
                        out,
                        "recap.enabled set to {b} (session-local; full persistence pending — #371 follow-up)"
                    );
                }
                Err(_) => {
                    let _ = writeln!(out, "usage: /config recap.enabled <true|false>");
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("recap.interval") {
            let val = rest.trim();
            match val.parse::<usize>() {
                Ok(n) if n >= 1 => {
                    let _ = writeln!(
                        out,
                        "recap.interval set to {n} (session-local; full persistence pending — #371 follow-up)"
                    );
                }
                _ => {
                    let _ = writeln!(out, "usage: /config recap.interval <positive integer>");
                }
            }
        } else {
            let _ = writeln!(out, "unknown /config key: {trimmed}");
            let _ = writeln!(out, "supported keys: recap.enabled, recap.interval");
        }
    }
}
