//! Ratatui-based interactive REPL — the primary user interface for open-mpm.
//!
//! Why: Ratatui's declarative render model eliminates the cursor-positioning
//! bug class that plagued the previous crossterm-based implementation
//! (ghost frames, duplicate banners, blank-row gaps, status-bar fights).
//! What: `OpenMpmRepl::run()` enters alt-screen via `tui::run_tui` and routes
//! every submitted line through `ReplBridge` which reuses the slash-command
//! table and `attempt_forward` LLM dispatch.
//! Test: `repl_skips_when_not_a_tty` confirms the non-interactive bypass;
//! `scripts/tmux-repl-test.sh` exercises the live PTY end-to-end.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::ctrl::{ctrl_socket_path, cwd_project_id};
use crate::identity::user_profile::UserProfile;

mod agent_commands;
mod banner;
mod bridge;
mod commands;
mod dispatch;
// Why: Event renderer staged for the REPL streaming loop but not yet routed
// through the active rendering path. Keep so the public API stays stable
// when the streaming integration lands.
#[allow(dead_code)]
mod event_display;
// Why: Lightweight ANSI markdown renderer prepared for use by the REPL chat
// printer but not yet wired into the active rendering path. Keep the module
// (with its self-contained tests) so the implementation is ready when the
// REPL adds markdown rendering. The `dead_code` allow suppresses warnings
// for the not-yet-invoked public helpers.
#[allow(dead_code)]
mod markdown;
mod ollama;
mod run;
pub(crate) mod status_bar;
pub(crate) mod statusline;
pub(crate) mod tui;

#[cfg(test)]
mod tests;

use status_bar::StatusBar;

/// A single rendered chat entry, kept in memory so we can redraw the chat
/// scrollback area between turns.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct ChatEntry {
    pub(crate) user: String,
    pub(crate) response: String,
    pub(crate) is_error: bool,
}

/// The interactive REPL.
pub struct OpenMpmRepl {
    pub(crate) user: Option<UserProfile>,
    pub(crate) project_name: String,
    pub(crate) socket_path: PathBuf,
    pub(crate) history_path: PathBuf,
    /// Resolved project directory — drives all `.open-mpm/*` lookups and
    /// task forwarding. Defaults to plain cwd at startup; the user attaches
    /// a real project via `/connect <path>`, which updates this field.
    pub(crate) project_dir: PathBuf,
    /// Path to the directory whose `*.toml` files seed the completer.
    pub(crate) agents_dir: PathBuf,
    /// Path to the skills directory (used by `/skills`).
    pub(crate) skills_dir: PathBuf,
    /// Short (8-char) UUID identifying this REPL session.
    #[allow(dead_code)]
    pub(crate) session_id: String,
    /// Current git branch captured once at startup. None outside a repo.
    #[allow(dead_code)]
    pub(crate) git_branch: Option<String>,
    /// Wall-clock anchor for the session.
    #[allow(dead_code)]
    pub(crate) session_start: std::time::Instant,
    /// Status bar rendered after each task completes.
    pub(crate) status_bar: StatusBar,
    /// Running multi-turn conversation with the PM/CTRL controller.
    pub(crate) conversation_history: Vec<crate::ctrl::ConversationTurn>,
    /// Rendered chat entries.
    #[allow(dead_code)]
    pub(crate) chat_log: Vec<ChatEntry>,
    /// Active persona agent (set via `/agent <name>`).
    pub(crate) active_persona: Option<String>,
    /// Background handle for the Telegram bot when started via `/telegram`.
    pub(crate) telegram_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shared pairing-code map used by `/telegram pair` (REPL) and the bot.
    ///
    /// Why (#334): The pairing code is generated in the trusted REPL and
    /// validated by the Telegram bot. Sharing this map is the IPC: the REPL
    /// writes a sentinel-keyed entry; the bot reads it on `/pair <code>`.
    /// This prevents Telegram-side attackers from self-authorizing.
    /// What: `crate::telegram::PendingPairs` (Arc-shared HashMap). Created
    /// at construction so the REPL can issue codes even before the bot is
    /// started.
    pub(crate) telegram_pairing: crate::telegram::PendingPairs,
    /// Background handle for the Slack bot when started via `/slack` (#452).
    pub(crate) slack_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shared pairing-code map used by `/slack pair` (REPL) and the bot.
    ///
    /// Why (#452): Mirrors the Telegram pairing IPC — codes are minted in the
    /// trusted REPL and validated by the Slack bot's `/slack-pair` handler.
    /// Arc-shared so codes generated in the REPL are visible to the bot task
    /// without restarting either side.
    pub(crate) slack_pairing: crate::slack::PendingPairs,
    /// Session-scoped model override (set via `/model <id>`). Applied at
    /// dispatch time to `cfg.agent.model`. None → use the value from agent TOML.
    /// Cleared by `/clear`, `/connect`, and `/model reset`.
    pub(crate) model_override: Option<String>,
    /// Session-scoped provider override (set via `/provider <name>`). One of
    /// "claude-code", "bedrock", "openrouter". Applied via
    /// `resolve_overridden_credentials` at dispatch, bypassing the env probe.
    /// Cleared by `/clear`, `/connect`, and `/provider reset`.
    pub(crate) provider_override: Option<String>,
    /// Cached ollama model list from the last `/provider local` probe.
    ///
    /// Why: The model picker (opened via `/model` with no arg) consults this
    /// when `provider_override == Some("local")` so the user sees actual
    /// locally-pulled models instead of the hardcoded Anthropic list.
    /// What: Refreshed by `handle_provider_local_into` on every successful
    /// probe; never cleared (stale data is harmless — picker still works).
    /// Test: Manual via `/provider local` -> `/model`.
    pub(crate) ollama_models: Vec<String>,
    /// TM (tmux manager) handle — always initialized (#319). When tmux is
    /// missing, the underlying orchestrator runs in degraded mode and
    /// individual `/tm` commands surface a clear runtime error.
    /// Issue #316 / #319.
    pub(crate) tm_manager: Arc<tokio::sync::Mutex<crate::tm::TmManager>>,
    /// Background idle monitor. Always running per #319 (was opt-in per #318).
    /// Polls every 30s; aborted automatically when the REPL drops.
    #[allow(dead_code)]
    pub(crate) tm_monitor: crate::tm::TmMonitor,
    /// When `Some`, the REPL is operating as a thin client against a
    /// running `open-mpm --serve` daemon (#343). User messages are
    /// forwarded over HTTP via `crate::service::submit_task_via_service`
    /// instead of running in-process. Set via `set_service_client_mode`.
    pub(crate) service_url: Option<String>,
}

impl OpenMpmRepl {
    /// Create a REPL configured for the current project.
    pub fn new(user: Option<UserProfile>) -> Result<Self> {
        let project_name = "ctrl".to_string();
        let socket_path = ctrl_socket_path(&cwd_project_id());

        let home = dirs::home_dir().context("no home directory")?;
        let history_path = home.join(".open-mpm").join("repl_history.txt");
        if let Some(parent) = history_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        // Why bug fix (#statusline-shows-sonnet): default project_dir to the
        // bundled open-mpm project root when the REPL is launched standalone
        // (i.e. cwd has no `.open-mpm/`). Falls through `OPEN_MPM_PROJECT_DIR`
        // and the canonical `detect_self_project` walk so the statusline
        // resolves the bundled `ctrl.toml` (haiku) instead of an unrelated
        // user-level config.
        let project_dir = std::env::current_dir()
            .ok()
            .filter(|d| d.join(".open-mpm").join("agents").is_dir())
            .or_else(crate::ctrl::detect_self_project)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let agents_dir = project_dir.join(".open-mpm").join("agents");
        let skills_dir = project_dir.join(".open-mpm").join("skills");

        let git_branch = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                }
            });

        let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let session_start = std::time::Instant::now();
        let status_bar = StatusBar::new("anthropic/claude-haiku-4-5", session_start);

        // Issue #316/#319: TM is always-on infrastructure. Initialize the
        // tmux manager rooted at the project's `.open-mpm/state/` directory.
        // The orchestrator degrades gracefully when tmux is missing — `/tm`
        // commands surface the runtime error per-command.
        let state_dir = project_dir.join(".open-mpm").join("state");
        // Best-effort: ignore failures here (the directory may already exist
        // or be unwritable in test contexts; TmManager will report below).
        let _ = std::fs::create_dir_all(&state_dir);
        let tm_manager = Arc::new(tokio::sync::Mutex::new(
            crate::tm::TmManager::new(&state_dir)
                .with_context(|| format!("initializing TM manager at {}", state_dir.display()))?,
        ));
        tracing::info!("TM: tmux manager initialized at {}", state_dir.display());

        // #319: the idle monitor is always running. 30s poll interval keeps
        // the registry honest without burning CPU. The monitor's Drop impl
        // aborts the polling task when the REPL tears down.
        let tm_monitor =
            crate::tm::TmMonitor::start(Arc::clone(&tm_manager), Duration::from_secs(30));

        Ok(Self {
            user,
            project_name,
            socket_path,
            history_path,
            project_dir,
            agents_dir,
            skills_dir,
            session_id,
            git_branch,
            session_start,
            status_bar,
            conversation_history: Vec::new(),
            chat_log: Vec::new(),
            active_persona: None,
            telegram_handle: None,
            telegram_pairing: crate::telegram::new_pending_pairs(),
            slack_handle: None,
            slack_pairing: crate::slack::new_pending_pairs(),
            model_override: None,
            provider_override: None,
            ollama_models: Vec::new(),
            tm_manager,
            tm_monitor,
            service_url: None,
        })
    }

    /// Switch this REPL into thin-client mode against a running service.
    ///
    /// Why (#343): When `open-mpm` is launched and a daemonized `--serve`
    /// is already running, the REPL should dispatch user messages over
    /// HTTP rather than spinning up its own controller. This setter is
    /// called from `main.rs` after a successful health probe.
    /// What: Stores the base URL (e.g. `http://localhost:8080`) used by
    /// `attempt_forward` to route POSTs to `/api/task`.
    /// Test: Manual via `open-mpm --service start && open-mpm`.
    pub fn set_service_client_mode(&mut self, url: impl Into<String>) {
        self.service_url = Some(url.into());
    }

    /// Borrow the shared Telegram-pairing handle.
    ///
    /// Why (#334): main.rs auto-starts the Telegram bot in parallel with the
    /// REPL. It needs to share the same `PendingPairs` map so codes issued by
    /// `/telegram pair` in the REPL are visible to the bot's `/pair` handler.
    /// What: Returns a clone of the inner `Arc` — cheap and safe to spawn.
    /// Test: Indirectly exercised by main.rs auto-start; see #334 PR.
    pub fn telegram_pairing_handle(&self) -> crate::telegram::PendingPairs {
        self.telegram_pairing.clone()
    }

    /// Borrow the shared Slack-pairing handle (#452).
    ///
    /// Why: Symmetric to `telegram_pairing_handle` — exposes the Arc-shared
    /// `PendingPairs` map so an externally-spawned Slack bot (e.g. when
    /// started via the `--slack` CLI flag) can share pairing state with the
    /// REPL's `/slack pair` command.
    pub fn slack_pairing_handle(&self) -> crate::slack::PendingPairs {
        self.slack_pairing.clone()
    }

    /// Resolve the on-disk paths the statusline should consult for the active
    /// agent, in priority order. Mirrors the dispatch-time resolution used by
    /// `run_pm_task_with_persona("ctrl", …)` and `resolve_agent_config(...)`
    /// so the statusline always shows what the NEXT dispatch will actually
    /// use — not an arbitrary first-found TOML.
    ///
    /// Why bug fix (#statusline-shows-sonnet): previously iterated
    /// `["ctrl.toml", "pm.toml"]` against `project_dir` only. When the user
    /// launched from a directory without `.open-mpm/` but had a user-level
    /// `~/.open-mpm/agents/ctrl.toml` with sonnet, the project path missed,
    /// the user-level was never consulted, and the fallback returned haiku —
    /// EXCEPT when project_dir happened to BE the user's home (which holds a
    /// stale sonnet ctrl.toml), at which point sonnet leaked into the
    /// statusline. Either way the value didn't match what dispatch loads.
    /// Now: ctrl mode walks project ctrl → user ctrl. PM mode walks project
    /// pm → user ctrl → project ctrl, matching `resolve_agent_config`.
    pub(crate) fn agent_toml_search_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(3);
        if self.project_name == "pm" {
            paths.push(
                self.project_dir
                    .join(".open-mpm")
                    .join("agents")
                    .join("pm.toml"),
            );
            if let Some(home) = dirs::home_dir() {
                paths.push(home.join(".open-mpm").join("agents").join("ctrl.toml"));
            }
            paths.push(
                self.project_dir
                    .join(".open-mpm")
                    .join("agents")
                    .join("ctrl.toml"),
            );
        } else {
            // ctrl persona: matches run_pm_task_with_persona("ctrl", …) order.
            paths.push(
                self.project_dir
                    .join(".open-mpm")
                    .join("agents")
                    .join("ctrl.toml"),
            );
            if let Some(home) = dirs::home_dir() {
                paths.push(home.join(".open-mpm").join("agents").join("ctrl.toml"));
            }
        }
        paths
    }

    /// Best-effort resolve of the runner kind declared by the active ctrl/PM
    /// agent TOML, used to gate claude-code credential routing for the
    /// statusline display (#295).
    pub(crate) fn resolve_active_runner(&self) -> crate::agents::RunnerKind {
        for p in self.agent_toml_search_paths() {
            if let Ok(s) = std::fs::read_to_string(&p) {
                for line in s.lines() {
                    let l = line.trim();
                    if let Some(rest) = l.strip_prefix("runner")
                        && let Some(eq) = rest.find('=')
                    {
                        let val = rest[eq + 1..].trim().trim_matches('"');
                        return match val {
                            "claude-code" => crate::agents::RunnerKind::ClaudeCode,
                            "in-process" => crate::agents::RunnerKind::InProcess,
                            "inline" => crate::agents::RunnerKind::Inline,
                            _ => crate::agents::RunnerKind::Subprocess,
                        };
                    }
                }
                // Found a TOML for this slot but no runner line → default.
                return crate::agents::RunnerKind::Subprocess;
            }
        }
        crate::agents::RunnerKind::Subprocess
    }

    /// Best-effort resolve of the agent model active for the ctrl/PM path.
    pub(crate) fn resolve_active_model(&self) -> String {
        for p in self.agent_toml_search_paths() {
            if let Ok(s) = std::fs::read_to_string(&p) {
                for line in s.lines() {
                    let l = line.trim();
                    if let Some(rest) = l.strip_prefix("model")
                        && let Some(eq) = rest.find('=')
                    {
                        let val = rest[eq + 1..].trim();
                        return val.trim_matches('"').to_string();
                    }
                }
                // Found a TOML for this slot but no model line → keep walking.
            }
        }
        "anthropic/claude-haiku-4-5".to_string()
    }

    /// Approximate count of native ctrl tools.
    pub(crate) fn count_active_tools(&self) -> usize {
        let base = 11_usize;
        let mcp = crate::tools::mcp_tools::mcp_tool_executors().len();
        base + mcp
    }

    /// Return the `AgentScope` for the current REPL state.
    ///
    /// Why: Multiple event-emit sites need to know the current scope after a
    ///   slash command mutates the REPL. Centralising the derivation here keeps
    ///   the rule in one place and avoids duplicating the logic across call sites.
    /// What: User if a persona is active (persona agents are always user-scoped),
    ///   User if project_name is "ctrl", Project otherwise (connected project).
    /// Test: Covered indirectly via `ReplBridge` integration paths.
    pub fn current_scope(&self) -> tui::AgentScope {
        if self.active_persona.is_some() || self.project_name == "ctrl" {
            tui::AgentScope::User
        } else {
            tui::AgentScope::Project
        }
    }
}

/// Read the agent TOML directory and return the list of agent names (file
/// stems of `*.toml` files). Returns an empty vec on any I/O error.
///
/// Why: The REPL's `/agents` slash command needs to list agents independently
/// of any completer/UI. Lives here now that `input.rs` is gone (#268 P5).
/// What: Non-recursive directory walk; sorted output.
/// Test: `discover_agent_names_reads_toml_stems`.
pub fn discover_agent_names(agents_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(agents_dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names
}

/// Strip the leading `vendor/` prefix from a model id for compact display.
///
/// Why: The startup statusline source string formats the LLM segment as
/// `LLM: provider:model`. Showing `anthropic/claude-haiku-4-5` after the
/// `provider:` adds redundant text. This mirrors `tui::strip_vendor_prefix`
/// for the few call sites in this module.
/// What: Returns everything after the first `/`; unchanged when absent.
/// Test: Indirectly via `tmux-repl-test.sh` (statusline assertions).
fn strip_vendor_prefix_for_display(model: &str) -> String {
    match model.find('/') {
        Some(i) => model[i + 1..].to_string(),
        None => model.to_string(),
    }
}

/// Public helper so other modules can quickly check if a tty is available.
pub fn is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Module-level path util so tests can reference history path discovery.
#[allow(dead_code)]
fn default_history_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".open-mpm").join("repl_history.txt"))
}

impl std::fmt::Debug for OpenMpmRepl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenMpmRepl")
            .field("project", &self.project_name)
            .field("socket", &self.socket_path)
            .field("history", &self.history_path)
            .finish()
    }
}

#[allow(dead_code)]
fn _path_ref(p: &Path) -> &Path {
    p
}
