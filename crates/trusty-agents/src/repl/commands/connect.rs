//! Part of the `commands` module (split from the monolithic `commands.rs`
//! for the 500-line file cap — see #357). Holds an `impl TrustyAgentsRepl` block
//! for one slash-command handler group.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::repl::TrustyAgentsRepl;

impl TrustyAgentsRepl {
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
                let pm_toml = path.join(".trusty-agents").join("agents").join("pm.toml");
                if !pm_toml.exists() {
                    let _ = writeln!(
                        out,
                        "warning: no .trusty-agents/agents/pm.toml at {} — may not be an trusty-agents project",
                        path.display()
                    );
                }
                self.project_dir = path.clone();
                self.project_name = crate::ctrl::socket::project_id_from_path(&path);
                self.socket_path = crate::ctrl::socket::ctrl_socket_path(&self.project_name);
                self.agents_dir = path.join(".trusty-agents").join("agents");
                self.skills_dir = path.join(".trusty-agents").join("skills");
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
                "  adapters: claude-mpm, claude-code, codex, augment, gemini, trusty-agents, shell"
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

        let projects_dir = path.join(".trusty-agents").join("projects");
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
