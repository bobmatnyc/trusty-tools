//! `OpenMpmRepl::run` — the REPL entry point, split from `mod.rs` for the
//! 500-line file cap (#357). The remaining `OpenMpmRepl` methods live in
//! `mod.rs`; methods compose freely across `impl` blocks.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use super::bridge::ReplBridge;
use super::{OpenMpmRepl, banner, statusline, strip_vendor_prefix_for_display, tui};

impl OpenMpmRepl {
    /// Run the REPL loop until the user quits (Ctrl-D or `/exit`).
    pub async fn run(&mut self) -> Result<()> {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("REPL requires a TTY; falling back to legacy stdin loop");
        }
        // Persisted history → in-memory history.
        let mut history: Vec<String> = Vec::new();
        if let Ok(content) = std::fs::read_to_string(&self.history_path) {
            for line in content.lines() {
                if !line.trim().is_empty() {
                    history.push(line.to_string());
                }
            }
        }

        // Banner data.
        let user_label = self
            .user
            .as_ref()
            .map(|u| u.name.clone())
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "user".to_string());
        let git_commits = banner::recent_git_commits(3);

        // Startup status string. Token counts + cost are appended dynamically
        // at render time by `build_rich_statusline` (they live on `ReplApp`).
        // We intentionally drop the Tools/Skills/MCP counts and the
        // `Type /help for commands.` hint — see #293.
        let model = self.resolve_active_model();
        let _tool_count = self.count_active_tools();
        let llm_label =
            crate::llm::credentials::pick_credentials(Some(self.resolve_active_runner()))
                .map(|c| c.label())
                .unwrap_or("none");
        let _skills_count = banner::count_dir_entries(&self.skills_dir, "md");
        let _mcp_count = banner::count_active_mcp_services();
        // #295/#296: `provider:model` (colon-separated, no parens, no vendor
        // prefix) keeps the statusline tight and consistent with the activity
        // panel's row-2 model id.
        let status = format!(
            "✓ LLM: {}:{} · All systems go.",
            llm_label,
            strip_vendor_prefix_for_display(&model)
        );

        let project_name = self.project_name.clone();
        let placeholder = OpenMpmRepl::new(self.user.clone())?;
        let owned = std::mem::replace(self, placeholder);
        let shared = Arc::new(tokio::sync::Mutex::new(owned));

        let handler = Arc::new(ReplBridge {
            repl: shared.clone(),
        });

        // Determine initial scope. At startup the active agent is always
        // "ctrl" (user-level). A project scope is set later via /connect.
        let initial_scope = if project_name == "pm" {
            tui::AgentScope::Project
        } else {
            tui::AgentScope::User
        };

        // Probe statusline state (config + git + working dir) once at startup.
        // The handler emits `StatuslineUpdate` events to refresh model/provider
        // when the user runs `/model` or `/provider`.
        let working_dir_str = {
            let r = shared.lock().await;
            r.project_dir.display().to_string()
        };
        let project_dir_for_probe: PathBuf = {
            let r = shared.lock().await;
            r.project_dir.clone()
        };
        let statusline_cfg = statusline::StatuslineConfig::load(&project_dir_for_probe);
        let (git_branch, git_dirty) = statusline::probe_git(&project_dir_for_probe);
        let provider_label =
            crate::llm::credentials::pick_credentials(Some(self.resolve_active_runner()))
                .map(|c| c.label().to_string())
                .unwrap_or_else(|| "none".to_string());

        // Issue #319: run the startup TM reconcile here (we're in an async
        // context now) so the chat history and statusline reflect existing
        // tmux sessions before the first frame renders.
        let mut initial_chat_messages: Vec<String> = Vec::new();
        // #367: Surface the harness version in chat history so users always
        // have it visible without needing to run a command.
        initial_chat_messages.push(format!(
            "open-mpm v{} — type /help for commands",
            env!("CARGO_PKG_VERSION")
        ));
        let mut claude_mpm_session_count: usize = 0;
        let tm_session_count: usize = {
            let tm_arc = {
                let r = shared.lock().await;
                Arc::clone(&r.tm_manager)
            };
            let mgr = tm_arc.lock().await;
            match mgr.reconcile().await {
                Ok(report) => {
                    let sessions = mgr.registry.list_sessions().unwrap_or_default();
                    let total = sessions.len();
                    // #331: count sessions running the claude-mpm adapter so
                    // the statusline can surface a distinct MPM segment.
                    claude_mpm_session_count = sessions
                        .iter()
                        .filter(|s| s.adapter_type == crate::tm::project::AdapterType::ClaudeMpm)
                        .count();
                    if !report.added.is_empty() {
                        initial_chat_messages.push(format!(
                            "TM active — {} session{} discovered (run /tm list to view)",
                            report.added.len(),
                            if report.added.len() == 1 { "" } else { "s" }
                        ));
                    } else if total == 0 {
                        initial_chat_messages
                            .push("TM active — no existing sessions found".to_string());
                    } else {
                        initial_chat_messages.push(format!(
                            "TM active — managing {} session{} (run /tm list to view)",
                            total,
                            if total == 1 { "" } else { "s" }
                        ));
                    }
                    total
                }
                Err(e) => {
                    tracing::warn!("TM: startup reconcile failed: {e:#}");
                    initial_chat_messages
                        .push(format!("TM active — startup reconcile failed: {e:#}"));
                    0
                }
            }
        };

        // #319: Probe local inference availability eagerly at startup so the
        // user knows immediately whether Ollama is reachable. The lazy
        // OnceLock path still fires on the first qualifying turn, but the
        // startup probe ensures the status message and statusline are
        // populated before the first frame renders.
        let local_model: Option<String> = {
            let cfg = crate::mcp::GlobalConfig::load().await;
            let li = &cfg.local_inference;
            if li.enabled {
                let available = crate::local_inference::is_ollama_available(&li.ollama_host).await;
                if available {
                    let display = li
                        .model
                        .strip_prefix("ollama/")
                        .unwrap_or(&li.model)
                        .to_string();
                    initial_chat_messages.push(format!(
                        "Local inference: {} active (supplemental — intent classification and simple queries)",
                        li.model
                            .strip_prefix("ollama/")
                            .unwrap_or(&li.model)
                    ));
                    Some(display)
                } else {
                    initial_chat_messages.push(format!(
                        "Local inference: configured ({}) but Ollama not reachable — remote fallback active",
                        li.model
                    ));
                    None
                }
            } else {
                None
            }
        };

        let startup = tui::ReplStartup {
            project_name,
            user_label,
            git_commits,
            initial_status: Some(status),
            initial_history: history,
            initial_scope,
            model_name: model.clone(),
            provider_name: provider_label,
            working_dir: working_dir_str,
            git_branch,
            git_dirty,
            statusline_config: statusline_cfg,
            project_dir: project_dir_for_probe.clone(),
            initial_chat_messages,
            tm_session_count,
            claude_mpm_session_count,
            local_model,
        };

        let res = tui::run_tui(startup, handler).await;

        // Restore the (possibly mutated) instance back into `*self`.
        let mut guard = shared.lock().await;
        let restored = std::mem::replace(&mut *guard, OpenMpmRepl::new(None)?);
        *self = restored;

        res
    }
}
