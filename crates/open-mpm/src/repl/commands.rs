//! Slash-command dispatch + all `*_into` handler methods for the REPL.
//!
//! Why: `try_handle_slash` is the central jump table the REPL bridges
//! against. Keeping the dispatch and per-command handlers together — but
//! out of `mod.rs` — makes the command surface easy to scan without
//! drowning the lifecycle code.
//! What: `impl OpenMpmRepl` block hosting `try_handle_slash` plus every
//! `*_into` writer used by the dispatch arms. The `/config` and
//! `/service` arms are factored into `handle_config_command_into` and
//! `handle_service_command_into` to keep the dispatch body readable.
//! Test: Comprehensive coverage lives in `mod.rs::tests` (see
//! `try_handle_slash_*` cases) which still reach in here via `use super::*`.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::OpenMpmRepl;
use super::ollama::{ollama_host, probe_ollama};

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

    /// Connect to the controller socket and request a `status` envelope,
    /// writing all status output into `out` instead of stdout.
    pub(crate) async fn send_status_command_into(&self, out: &mut String) -> Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let stream = crate::ctrl::CtrlSocket::probe_default(&self.socket_path)
            .await
            .context("controller is not running on this project's socket")?;
        let (read_half, mut write_half) = stream.into_split();
        let id = uuid::Uuid::new_v4().to_string();
        let cmd = serde_json::json!({"type": "status", "id": id});
        let mut line = serde_json::to_string(&cmd)?;
        line.push('\n');
        write_half.write_all(line.as_bytes()).await?;
        write_half.flush().await?;

        let mut reader = tokio::io::BufReader::new(read_half);
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).await?;
            if n == 0 {
                break;
            }
            let v: serde_json::Value = match serde_json::from_str(buf.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("output") => {
                    if let Some(t) = v.get("text").and_then(|x| x.as_str()) {
                        let _ = writeln!(out, "{t}");
                    }
                }
                Some("done") => return Ok(()),
                Some("error") => {
                    let msg = v
                        .get("error")
                        .and_then(|x| x.as_str())
                        .unwrap_or("(no error)")
                        .to_string();
                    anyhow::bail!("{msg}");
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Switch project (shared by `/connect` and `/cd`), writing output into `out`.
    pub(crate) fn do_connect_into(&mut self, arg: &str, out: &mut String) {
        if arg.is_empty() {
            let _ = writeln!(out, "usage: /connect <project-path>");
            let _ = writeln!(out, "       /connect .   (use current directory)");
            return;
        }
        let raw = if arg == "." {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(arg)
        };
        match raw.canonicalize() {
            Err(e) => {
                let _ = writeln!(out, "error: cannot resolve path '{}': {}", arg, e);
            }
            Ok(path) => {
                let pm_toml = path.join(".open-mpm").join("agents").join("pm.toml");
                if !pm_toml.exists() {
                    let _ = writeln!(
                        out,
                        "warning: no .open-mpm/agents/pm.toml at {} — may not be an open-mpm project",
                        path.display()
                    );
                }
                self.project_dir = path.clone();
                self.project_name = crate::ctrl::socket::project_id_from_path(&path);
                self.socket_path = crate::ctrl::socket::ctrl_socket_path(&self.project_name);
                self.agents_dir = path.join(".open-mpm").join("agents");
                self.skills_dir = path.join(".open-mpm").join("skills");
                self.conversation_history.clear();
                self.chat_log.clear();
                self.active_persona = None;
                // #284: Project switch resets session overrides so the new
                // project's TOML defaults are honored from the first turn.
                self.model_override = None;
                self.provider_override = None;
                let _ = writeln!(out, "switched to project: {}", path.display());
            }
        }
    }

    /// `/connect <path> <adapter> [name]` — create-or-reuse a TM project
    /// config and spawn a `<name>-<adapter>-<serial>` tmux session (#451).
    ///
    /// Why: This replaces the legacy `/connect` project-switcher (now `/cd`).
    /// The new harness model treats `/connect` as "wire a project up to a tmux
    /// session via the named adapter", which is the same operation the WebUI
    /// "Add Project" form performs through `POST /api/projects`. Both paths
    /// converge on `TmManager::connect_or_create` so the on-disk shape and
    /// session naming are identical regardless of entry point.
    /// What: Parses up to three whitespace-separated args, expands `~` and
    /// `.` in the path, canonicalizes it, and delegates to
    /// `TmManager::connect_or_create`. On success prints the new session name
    /// plus the `tmux attach-session -t` invocation; on failure surfaces the
    /// underlying error string.
    /// Test: Behavior is covered by `TmManager::connect_or_create` and
    /// `ProjectConfigStore::find_or_create` unit tests; the REPL wiring is
    /// exercised manually (requires a live tmux server).
    pub(crate) async fn do_connect_tm_into(&mut self, arg: &str, out: &mut String) {
        let parts: Vec<&str> = arg.split_whitespace().collect();
        if parts.len() < 2 {
            let _ = writeln!(out, "usage: /connect <path> <adapter> [name]");
            let _ = writeln!(
                out,
                "  adapters: claude-mpm, claude-code, codex, augment, gemini, open-mpm, shell"
            );
            let _ = writeln!(
                out,
                "  hint: use `/cd <path>` to switch the REPL's project context without spawning a session"
            );
            return;
        }
        let raw_path = parts[0];
        let adapter = parts[1];
        let name_override = parts.get(2).map(|s| s.to_string());

        let expanded = expand_tilde(raw_path);
        let path = match expanded.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                let _ = writeln!(out, "error: cannot resolve path '{}': {}", raw_path, e);
                return;
            }
        };

        let projects_dir = path.join(".open-mpm").join("projects");
        let mgr = self.tm_manager.lock().await;
        match mgr
            .connect_or_create(&projects_dir, &path, adapter, name_override.as_deref())
            .await
        {
            Ok((cfg, session)) => {
                let _ = writeln!(out, "Connected: {}", session.name);
                let _ = writeln!(out, "Project:   {} ({})", cfg.project.name, path.display());
                let _ = writeln!(out, "To attach: tmux attach-session -t {}", session.name);
            }
            Err(e) => {
                let _ = writeln!(out, "error: {e}");
            }
        }
    }

    /// Print known projects from the global registry into `out` (#340).
    pub(crate) async fn print_projects_into(&self, out: &mut String, show_all: bool) {
        let _ = writeln!(
            out,
            "Current project: {}  ({})",
            self.project_name,
            self.project_dir.display()
        );

        let reg = match crate::registry::ProjectRegistry::new() {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(out, "registry: {e}");
                return;
            }
        };
        let entries_map = match reg.load().await {
            Ok(m) => m,
            Err(e) => {
                let _ = writeln!(out, "registry: {e}");
                return;
            }
        };
        if entries_map.is_empty() {
            let _ = writeln!(out, "\nNo projects registered yet.");
            let _ = writeln!(out, "Use /connect <path> to switch projects.");
            return;
        }
        let entries: Vec<crate::registry::ProjectEntry> = entries_map.into_values().collect();

        // Gather live tmux sessions so we can correlate them and broaden the
        // active set with session-owning projects.
        let sessions = {
            let mgr = self.tm_manager.lock().await;
            mgr.list_sessions().await.unwrap_or_default()
        };
        let session_paths: Vec<std::path::PathBuf> =
            sessions.iter().map(|s| s.project_path.clone()).collect();

        let header = if show_all {
            format!("\nAll projects ({}):", entries.len())
        } else {
            "\nActive projects (past 14 days + tmux sessions):".to_string()
        };
        let _ = writeln!(out, "{header}");
        let _ = writeln!(out);

        let display: Vec<&crate::registry::ProjectEntry> = if show_all {
            let mut all: Vec<&crate::registry::ProjectEntry> =
                entries.iter().filter(|e| e.is_real_project()).collect();
            all.sort_by_key(|b| std::cmp::Reverse(b.last_active()));
            all
        } else {
            crate::registry::discover_active_projects(
                &entries,
                &session_paths,
                chrono::Duration::days(14),
            )
        };

        if display.is_empty() {
            let _ = writeln!(out, "  (no recently active projects — try /projects --all)");
        }

        for entry in &display {
            let marker = if entry.path == self.project_dir {
                "*"
            } else {
                " "
            };
            // Render `~` for $HOME prefix to keep paths readable.
            let path_str = entry.path.to_string_lossy().to_string();
            let _ = writeln!(out, "  {marker} {}  {}", entry.name, path_str);

            // Origin / issue / PR / last-active line.
            let mut detail_parts: Vec<String> = Vec::new();
            if let Some(origin) = entry.git_origin.as_deref() {
                if let Some(repo) = crate::registry::extract_github_repo(origin) {
                    detail_parts.push(format!("origin: github.com/{repo}"));
                } else {
                    detail_parts.push(format!("origin: {origin}"));
                }
            }
            if let Some(n) = entry.open_issues_count {
                detail_parts.push(format!("{n} issues"));
            }
            if let Some(n) = entry.open_prs_count {
                detail_parts.push(format!("{n} PRs"));
            }
            if let Some(t) = entry.last_active() {
                let secs = (chrono::Utc::now() - t).num_seconds().max(0);
                let ago = if secs < 60 {
                    format!("{secs}s ago")
                } else if secs < 3600 {
                    format!("{}m ago", secs / 60)
                } else if secs < 86400 {
                    format!("{}h ago", secs / 3600)
                } else {
                    format!("{}d ago", secs / 86400)
                };
                detail_parts.push(format!("active {ago}"));
            }
            if !detail_parts.is_empty() {
                let _ = writeln!(out, "    {}", detail_parts.join(" · "));
            }

            // Sessions owned by this project.
            let mine: Vec<_> = sessions
                .iter()
                .filter(|s| s.project_path == entry.path)
                .collect();
            if mine.is_empty() {
                let _ = writeln!(out, "    (no active sessions)");
            } else {
                let line = mine
                    .iter()
                    .map(|s| format!("{} [{}] {:?}", s.name, entry.name, s.status))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "    sessions: {line}");
            }
            let _ = writeln!(out);
        }

        let _ = writeln!(
            out,
            "Use /connect <path> to switch.  /projects --all to show all."
        );
    }

    /// Feature B5: Print the last 20 NDJSON entries from today's chat log.
    pub(crate) fn print_recent_logs_into(&self, out: &mut String) {
        let logger = match crate::logging::global() {
            Some(l) => l,
            None => {
                let _ = writeln!(out, "logs: chat logging is not enabled.");
                return;
            }
        };
        let path = logger.today_log_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                let _ = writeln!(out, "logs: no entries yet today ({}).", path.display());
                return;
            }
        };
        let lines: Vec<&str> = raw.lines().collect();
        let start = lines.len().saturating_sub(20);
        let _ = writeln!(
            out,
            "Recent chat log entries ({} shown of {} today):",
            lines.len() - start,
            lines.len()
        );
        for line in &lines[start..] {
            let parsed: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ts = parsed
                .get("ts")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.format("%H:%M").to_string())
                .unwrap_or_else(|| "--:--".to_string());
            let role = parsed
                .get("role")
                .and_then(|v| v.as_str())
                .or_else(|| parsed.get("tool").and_then(|v| v.as_str()))
                .unwrap_or("?");
            let content = parsed
                .get("content")
                .and_then(|v| v.as_str())
                .or_else(|| parsed.get("output").and_then(|v| v.as_str()))
                .unwrap_or("");
            let preview: String = content
                .chars()
                .take(80)
                .collect::<String>()
                .replace('\n', " ");
            let _ = writeln!(out, "[{ts}] {role}: {preview}");
        }
    }

    /// Tail the last N lines of the perf runs log into `out`.
    pub(crate) fn tail_log_into(&self, n: usize, out: &mut String) {
        let log_path = self
            .project_dir
            .join("docs")
            .join("performance")
            .join("runs.log");
        match std::fs::read_to_string(&log_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = lines.len().saturating_sub(n);
                for line in &lines[start..] {
                    let _ = writeln!(out, "{line}");
                }
            }
            Err(e) => {
                let _ = writeln!(out, "log: cannot read {}: {e}", log_path.display());
            }
        }
    }

    /// Print last N entries from the REPL input history file into `out`.
    pub(crate) fn print_history_into(&self, n: usize, out: &mut String) {
        match std::fs::read_to_string(&self.history_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let _ = writeln!(out, "REPL input history (last {n} of {}):", lines.len());
                let start = lines.len().saturating_sub(n);
                for (i, line) in lines[start..].iter().enumerate() {
                    let _ = writeln!(out, "{:4}  {line}", start + i + 1);
                }
            }
            Err(e) => {
                let _ = writeln!(out, "history: {e}");
            }
        }
    }

    /// Handle `/provider [<name>|reset]` slash command (#284).
    pub(crate) fn handle_provider_command_into(&mut self, arg: &str, out: &mut String) {
        const VALID: &[&str] = &["openrouter", "claude-code", "bedrock", "local"];
        // Bookmark: future support for "anthropic-api" and "openai-api" goes here.
        // Note: "local" is handled by `handle_provider_local_into` (async, probes ollama).

        if arg.is_empty() {
            match self.provider_override.as_deref() {
                Some(p) => {
                    let _ = writeln!(out, "Provider: {} (session override)", p);
                }
                None => {
                    let _ = writeln!(out, "Provider: default (auto from env)");
                }
            }
            let _ = writeln!(out, "Valid: {} (or 'reset')", VALID.join(", "));
            return;
        }
        if arg == "reset" {
            self.provider_override = None;
            let _ = writeln!(out, "Provider reset to default (auto from env)");
            return;
        }
        if VALID.contains(&arg) {
            self.provider_override = Some(arg.to_string());
            let _ = writeln!(out, "Provider set to: {}", arg);
        } else {
            let _ = writeln!(
                out,
                "Unknown provider: {}. Valid: {}",
                arg,
                VALID.join(", ")
            );
        }
    }

    /// Handle `/provider local` — probe ollama and switch to local routing.
    pub(crate) async fn handle_provider_local_into(&mut self, out: &mut String) {
        let host = ollama_host();
        match probe_ollama(&host).await {
            Err(e) => {
                let _ = writeln!(
                    out,
                    "ollama not running at {} (set OLLAMA_HOST to override)",
                    host
                );
                let _ = writeln!(out, "details: {e:#}");
            }
            Ok(models) if models.is_empty() => {
                let _ = writeln!(
                    out,
                    "ollama is running at {} but has no models pulled. Run e.g. `ollama pull llama3.2` and retry.",
                    host
                );
            }
            Ok(models) => {
                self.provider_override = Some("local".to_string());
                // Cache for the next `/model` picker so it shows actual
                // locally-pulled models.
                self.ollama_models = models.clone();
                let _ = writeln!(out, "ollama running at {host}. Available models:");
                for m in &models {
                    let _ = writeln!(out, "  {m}");
                }
                let _ = writeln!(out, "Use /model <name> to select.");
            }
        }
    }

    /// Handle `/local [on|off|test]` slash command (#319).
    pub(crate) async fn handle_local_command_into(&mut self, arg: &str, out: &mut String) {
        let arg = arg.trim();
        match arg {
            "on" => {
                let mut cfg = crate::mcp::GlobalConfig::load_or_create()
                    .await
                    .unwrap_or_default();
                cfg.local_inference.enabled = true;
                if let Err(e) = cfg.save().await {
                    let _ = writeln!(out, "failed to persist config: {e:#}");
                    return;
                }
                let _ = writeln!(out, "Local inference: ENABLED");
                let _ = writeln!(out, "Model: {}", cfg.local_inference.model);
                let _ = writeln!(out, "Probing {}...", cfg.local_inference.ollama_host);
                let ok = crate::local_inference::probe_ollama_now(&cfg.local_inference.ollama_host)
                    .await;
                if ok {
                    let _ = writeln!(out, "Ollama: reachable");
                } else {
                    let _ = writeln!(out, "Ollama: NOT reachable — start with `ollama serve`");
                }
                return;
            }
            "off" => {
                let mut cfg = crate::mcp::GlobalConfig::load_or_create()
                    .await
                    .unwrap_or_default();
                cfg.local_inference.enabled = false;
                if let Err(e) = cfg.save().await {
                    let _ = writeln!(out, "failed to persist config: {e:#}");
                    return;
                }
                let _ = writeln!(out, "Local inference: DISABLED");
                return;
            }
            "test" => {
                let cfg = crate::mcp::GlobalConfig::load().await;
                let host = &cfg.local_inference.ollama_host;
                let _ = writeln!(out, "Probing {}...", host);
                let ok = crate::local_inference::probe_ollama_now(host).await;
                if ok {
                    let _ = writeln!(out, "Ollama: reachable at {}", host);
                    match probe_ollama(host).await {
                        Ok(models) if !models.is_empty() => {
                            let _ = writeln!(out, "Available models:");
                            for m in models.iter().take(20) {
                                let _ = writeln!(out, "  {}", m);
                            }
                        }
                        Ok(_) => {
                            let _ = writeln!(
                                out,
                                "(no models pulled — run e.g. `ollama pull qwen3:30b`)"
                            );
                        }
                        Err(e) => {
                            let _ = writeln!(out, "Failed to list models: {e:#}");
                        }
                    }
                } else {
                    let _ = writeln!(
                        out,
                        "Ollama: NOT reachable at {} — start with `ollama serve`",
                        host
                    );
                }
                return;
            }
            "" => {} // fall through to status display
            other => {
                let _ = writeln!(
                    out,
                    "unknown /local subcommand: {other}\nusage: /local [on|off|test]"
                );
                return;
            }
        }

        // Status display.
        let cfg = crate::mcp::GlobalConfig::load().await;
        let li = &cfg.local_inference;
        let _ = writeln!(out, "Local Inference (Ollama)");
        let _ = writeln!(
            out,
            "  Status:   {}",
            if li.enabled { "enabled" } else { "disabled" }
        );
        let _ = writeln!(out, "  Model:    {}", li.model);
        let _ = writeln!(out, "  Host:     {}", li.ollama_host);
        let _ = writeln!(
            out,
            "  Fallback: {}",
            if li.fallback_on_error { "on" } else { "off" }
        );
        let _ = writeln!(out, "  Max tokens: {}", li.max_tokens);

        // Probe ollama for live status.
        let reachable = crate::local_inference::probe_ollama_now(&li.ollama_host).await;
        let _ = writeln!(
            out,
            "  Ollama:   {}",
            if reachable {
                "reachable"
            } else {
                "NOT reachable (run `ollama serve`)"
            }
        );

        if reachable
            && let Ok(models) = probe_ollama(&li.ollama_host).await
            && !models.is_empty()
        {
            let _ = writeln!(out);
            let _ = writeln!(out, "Available models:");
            for m in models.iter().take(20) {
                let _ = writeln!(out, "  {}", m);
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Toggle with `/local on` / `/local off`, or edit ~/.open-mpm/config.toml [local_inference]."
        );
    }

    /// Handle `/model [<id>|reset]` slash command (#284).
    pub(crate) fn handle_model_command_into(&mut self, arg: &str, out: &mut String) {
        if arg.is_empty() {
            match self.model_override.as_deref() {
                Some(m) => {
                    let _ = writeln!(out, "Model: {} (session override)", m);
                }
                None => {
                    let m = self.resolve_active_model();
                    let _ = writeln!(out, "Model: {} (from agent TOML)", m);
                }
            }
            let _ = writeln!(out, "Usage: /model <id> | /model reset");
            return;
        }
        if arg == "reset" {
            self.model_override = None;
            // Status bar shows the TOML-resolved model when no override active.
            self.status_bar.model = self.resolve_active_model();
            let _ = writeln!(out, "Model reset to default");
            return;
        }
        self.model_override = Some(arg.to_string());
        self.status_bar.model = arg.to_string();
        let _ = writeln!(out, "Model set to: {}", arg);
    }

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

/// Print the slash-command reference.
pub(crate) fn write_help(out: &mut String) {
    let _ = writeln!(
        out,
        "open-mpm REPL — slash commands

  Navigation
    /connect <path> <adapter> [name]
                         Create-or-reuse a TM project config and spawn a
                         `<name>-<adapter>-<serial>` tmux session.
                         adapters: claude-mpm, claude-code, codex, augment,
                         gemini, open-mpm, shell
    /cd <path>           Switch the REPL's project context (.open-mpm root)
                         without spawning a tmux session
    /projects            Show current project + how to switch

  Information
    /version             Build version and number
    /status              Controller liveness
    /session             Session ID, socket, project path
    /agent [<name>]      Switch persona, or list assistant agents
    /switch [<name>]     Switch front-end voice (ctrl | Izzie | CTO Assistant)
    /agents              List available agents
    /skills              List available skills
    /memories [query]    Search the memory store
    /history [N]         Show last N REPL input history entries (default 10)

  Actions
    /run <file>          Forward task from a file
    /log [N]             Tail last N lines of perf log (default 20)
    /logs                Tail last 20 chat-log entries (today)
    /telegram [cmd]      Telegram bot gateway (start|stop|status|pair)
                         `pair` issues a one-time code shown only in the REPL
                         to authorize a Telegram chat (#334).
    /slack [cmd]         Slack bot gateway (start|stop|status|pair)
                         `pair` issues a one-time code shown only in the REPL
                         to authorize a Slack channel (#452).
    /tm <subcmd>         Tmux session manager (try `/tm help`)
    /service [start|stop|status]  Manage persistent --serve daemon (#343)
    /clear               Clear terminal and reset conversation history
    /update              Check GitHub for a newer release and upgrade in place (#368)
    /exit | /quit | /disconnect  Quit  (Ctrl-D also works)
                         `/disconnect` is preferred when attached to a server session —
                         same effect, clearer intent (server keeps running).

  Session Management (run from terminal, not REPL)
    om start             Start the API server daemon
    om stop              Stop the API server daemon
    om status            Show server status (port, PID, uptime)
    om connect <path>    Register a project with the running server
    om session new       --project <path> --name <name> [--agent <agent>] [--worktree]
    om session list      [<project-path>]
    om session attach    <session-id>
    om session kill      <session-id>

  Routing (session-scoped overrides)
    /provider [<name>]   Show or set credential routing (openrouter|claude-code|bedrock|local|reset)
                         `local` probes a running ollama (OLLAMA_HOST or http://localhost:11434)
    /model [<id>]        Show or set model id for this session (or reset)
    /local [on|off|test] Local Ollama fast-path status / control (#319)

Type any other text to send it as a task to the PM controller."
    );
}

/// Expand a leading `~` or bare `.` in a user-supplied path.
///
/// Why: The new `/connect` syntax (#451) accepts paths typed by humans, so it
/// needs to handle `~`, `~/foo`, and `.` the way a shell would before handing
/// the result to `canonicalize`. `std::path` deliberately does not do this.
/// What: Returns `$HOME` for `~`, `$HOME/<rest>` for `~/<rest>`, the current
/// working directory for `.`, and the input unchanged otherwise. `$HOME`
/// resolution falls back to the input on failure so `/connect` still reports
/// a sensible error from `canonicalize`.
/// Test: Indirect — covered by `/connect` smoke tests and the
/// `TmManager::connect_or_create` happy path.
fn expand_tilde(input: &str) -> PathBuf {
    if input == "." {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    }
    if input == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(input));
    }
    if let Some(rest) = input.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(input)
}
